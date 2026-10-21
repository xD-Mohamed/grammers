// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::mem;
use std::sync::Arc;
#[cfg(feature = "fs")]
use std::{io::SeekFrom, path::Path};

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use grammers_mtsender::InvocationError;
use grammers_tl_types as tl;
use tokio::io::{self, AsyncRead, AsyncReadExt};
use tokio::sync::Mutex as AsyncMutex;
#[cfg(feature = "fs")]
use tokio::{
    fs,
    io::{AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
};

use super::Client;
use crate::media::{Downloadable, Uploaded};
use crate::utils::generate_random_id;

pub const MIN_CHUNK_SIZE: i32 = 4 * 1024;
pub const MAX_CHUNK_SIZE: i32 = 512 * 1024;
const FILE_MIGRATE_ERROR: i32 = 303;
const BIG_FILE_SIZE: usize = 10 * 1024 * 1024;
const WORKER_COUNT: usize = 4;

fn unsupported_cdn() -> InvocationError {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Telegram redirected this file to a CDN; CDN downloads are not supported by this client",
    )
    .into()
}

fn migration_dc(value: Option<u32>) -> Result<i32, InvocationError> {
    value
        .and_then(|value| i32::try_from(value).ok())
        .filter(|dc| *dc > 0)
        .ok_or(InvocationError::InvalidDc)
}

#[cfg(feature = "fs")]
fn download_bytes(response: tl::enums::upload::File) -> Result<Vec<u8>, InvocationError> {
    match response {
        tl::enums::upload::File::File(file) => Ok(file.bytes),
        tl::enums::upload::File::CdnRedirect(_) => Err(unsupported_cdn()),
    }
}

#[cfg(feature = "fs")]
fn validate_download(size: usize, workers: usize) -> Result<(), io::Error> {
    if workers == 0 || i64::try_from(size).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "downloads require at least one worker and a size within the signed 64-bit offset range",
        ));
    }
    Ok(())
}

// Own each in-flight chunk future inside the download. Dropping this future, a failed
// request, or a disk error drops every child future. No download worker or result queue outlives
// the caller, and at most `workers` response chunks can be held while writing.
#[cfg(feature = "fs")]
async fn download_chunks<W, F, Fut>(
    file: &mut W,
    size: usize,
    workers: usize,
    fetch: F,
) -> Result<(), InvocationError>
where
    W: AsyncWrite + AsyncSeek + Unpin,
    F: Fn(i64) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, InvocationError>>,
{
    validate_download(size, workers)?;
    let mut offsets = (0..size).step_by(MAX_CHUNK_SIZE as usize);
    let mut pending = FuturesUnordered::new();
    let chunk = |offset: usize| {
        let request = fetch(offset as i64);
        async move { (offset, request.await) }
    };
    for offset in offsets.by_ref().take(workers) {
        pending.push(chunk(offset));
    }
    let mut position = 0;
    while let Some((offset, result)) = pending.next().await {
        let bytes = result?;
        let expected = (size - offset).min(MAX_CHUNK_SIZE as usize);
        if bytes.len() != expected {
            return Err(io::Error::new(
                if bytes.len() < expected {
                    io::ErrorKind::UnexpectedEof
                } else {
                    io::ErrorKind::InvalidData
                },
                "downloaded chunk length does not match the known file size",
            )
            .into());
        }
        if offset as u64 != position {
            file.seek(SeekFrom::Start(offset as u64)).await?;
        }
        file.write_all(&bytes).await?;
        position = offset as u64 + bytes.len() as u64;
        if let Some(offset) = offsets.next() {
            pending.push(chunk(offset));
        }
    }
    file.flush().await?;
    Ok(())
}

/// Iterator returned by [`Client::iter_download`].
pub struct DownloadIter {
    client: Client,
    done: bool,
    size: Option<usize>,
    variant: DownloadIterVariant,
    dc_id: Option<i32>,
}

enum DownloadIterVariant {
    Request(tl::functions::upload::GetFile),
    PreDownloaded(Vec<u8>),
    PreFailed(io::Error),
    Empty,
}

impl DownloadIter {
    /// Changes the chunk size, in bytes, used to make requests. Useful if you only need to get a
    /// small part of a file. By default, `MAX_CHUNK_SIZE` is used.
    ///
    /// # Panics
    ///
    /// Panics if `size` is not divisible by `MIN_CHUNK_SIZE`, or if `size` is not in contained in
    /// the range `MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE`.
    pub fn chunk_size(mut self, size: i32) -> Self {
        assert!((MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&size) && size % MIN_CHUNK_SIZE == 0);
        if let DownloadIterVariant::Request(request) = &mut self.variant {
            request.limit = size;
        }
        self
    }

    /// Skips `n` chunks to start downloading a different offset from the file. If you want to
    /// skip less data, modify the `chunk_size` before calling this method, and then reset it to
    /// any value you want.
    pub fn skip_chunks(mut self, n: i32) -> Self {
        if let DownloadIterVariant::Request(request) = &mut self.variant {
            request.offset += request.limit as i64 * (n as i64);
        }
        self
    }

    /// Fetch and return the next chunk.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, InvocationError> {
        if self.done {
            return Ok(None);
        }

        let variant = mem::replace(&mut self.variant, DownloadIterVariant::Empty);

        let mut request = match variant {
            DownloadIterVariant::Request(r) => r,
            DownloadIterVariant::PreDownloaded(data) => {
                self.done = true;
                return Ok(Some(data.clone()));
            }
            DownloadIterVariant::PreFailed(error) => {
                return Err(InvocationError::Io(error));
            }
            DownloadIterVariant::Empty => return Ok(None),
        };

        use tl::enums::upload::File;

        // TODO handle maybe FILEREF_UPGRADE_NEEDED
        let mut dc = self
            .dc_id
            .map_or_else(|| self.client.0.session.home_dc_id(), Ok)?;

        loop {
            break match self.client.invoke_in_dc(dc, &request).await {
                Ok(File::File(f)) => {
                    let reached_known_size = self
                        .size
                        .is_some_and(|size| request.offset as usize + f.bytes.len() >= size);

                    if reached_known_size || f.bytes.len() < request.limit as usize {
                        self.done = true;
                        if f.bytes.is_empty() {
                            break Ok(None);
                        }
                    }

                    request.offset += request.limit as i64;
                    self.variant = DownloadIterVariant::Request(request);

                    Ok(Some(f.bytes))
                }
                Ok(File::CdnRedirect(_)) => Err(unsupported_cdn()),
                Err(InvocationError::Rpc(err)) if &err.name == "AUTH_KEY_UNREGISTERED" => {
                    match self.client.copy_auth_to_dc(dc).await {
                        Ok(_) => continue,
                        Err(e) => Err(e),
                    }
                }
                Err(InvocationError::Rpc(err)) if err.code == FILE_MIGRATE_ERROR => {
                    dc = migration_dc(err.value)?;
                    continue;
                }
                Err(e) => Err(e),
            };
        }
    }
}

/// Method implementations related to uploading or downloading files.
impl Client {
    /// Returns a new iterator over the contents of a media document that will be downloaded.
    ///
    /// CDN redirects return [`InvocationError::Io`] with [`io::ErrorKind::Unsupported`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(downloadable: grammers_client::media::Media, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let mut file_bytes = Vec::new();
    /// let mut download = client.iter_download(&downloadable);
    ///
    /// while let Some(chunk) = download.next().await? {
    ///     file_bytes.extend(chunk);
    /// }
    ///
    /// // The file is now downloaded in-memory, inside `file_bytes`!
    /// # Ok(())
    /// # }
    /// ```
    pub fn iter_download<D: Downloadable>(&self, downloadable: &D) -> DownloadIter {
        if let Some(data) = downloadable.to_data() {
            DownloadIter {
                client: self.clone(),
                done: false,
                size: Some(data.len()),
                variant: DownloadIterVariant::PreDownloaded(data),
                dc_id: downloadable.dc_id(),
            }
        } else if let Some(location) = downloadable.to_raw_input_location() {
            DownloadIter {
                client: self.clone(),
                done: false,
                size: downloadable.size(),
                variant: DownloadIterVariant::Request(tl::functions::upload::GetFile {
                    precise: false,
                    cdn_supported: false,
                    location,
                    offset: 0,
                    limit: MAX_CHUNK_SIZE,
                }),
                dc_id: downloadable.dc_id(),
            }
        } else {
            DownloadIter {
                client: self.clone(),
                done: false,
                size: None,
                variant: DownloadIterVariant::PreFailed(io::Error::other("media not downloadable")),
                dc_id: downloadable.dc_id(),
            }
        }
    }

    /// Downloads a media file into the specified path.
    ///
    /// If the file already exists, it will be overwritten.
    ///
    /// This is a small wrapper around [`Client::iter_download`] for the common case of
    /// wanting to save the file locally.
    /// Large files use at most four concurrent chunk requests with bounded response storage.
    /// Cancelling the download or encountering an error drops all chunk futures and leaves
    /// a potentially incomplete destination file. Already accepted RPCs retain their normal
    /// sender lifetime; cancellation does not replay them or roll back already issued writes.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(downloadable: grammers_client::media::Media, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// client.download_media(&downloadable, "/home/username/photos/holidays.jpg").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "fs")]
    pub async fn download_media<D: Downloadable, P: AsRef<Path>>(
        &self,
        downloadable: &D,
        path: P,
    ) -> Result<(), InvocationError> {
        // Concurrent downloader
        if let Some((location, size)) = downloadable
            .to_raw_input_location()
            .zip(downloadable.size())
        {
            let dc_id = downloadable
                .dc_id()
                .map_or_else(|| self.0.session.home_dc_id(), Ok)?;
            if size > BIG_FILE_SIZE {
                return self
                    .download_media_concurrent(location, size, path, WORKER_COUNT, dc_id)
                    .await;
            }
        }

        let mut download = self.iter_download(downloadable);
        Client::load(path, &mut download).await
    }

    #[cfg(feature = "fs")]
    async fn load<P: AsRef<Path>>(
        path: P,
        download: &mut DownloadIter,
    ) -> Result<(), InvocationError> {
        let mut file = fs::File::create(path).await?;
        while let Some(chunk) = download.next().await? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    }

    /// Downloads a `Document` to specified path using multiple connections
    #[cfg(feature = "fs")]
    async fn download_media_concurrent<P: AsRef<Path>>(
        &self,
        location: tl::enums::InputFileLocation,
        size: usize,
        path: P,
        workers: usize,
        dc_id: i32,
    ) -> Result<(), InvocationError> {
        validate_download(size, workers)?;
        let mut file = fs::File::create(path).await?;
        file.set_len(size as u64).await?;

        download_chunks(&mut file, size, workers, |offset| {
            let client = self.clone();
            let location = location.clone();
            async move {
                let mut task_dc = dc_id;
                let request = tl::functions::upload::GetFile {
                    precise: true,
                    cdn_supported: false,
                    location,
                    offset,
                    limit: MAX_CHUNK_SIZE,
                };
                loop {
                    match client.invoke_in_dc(task_dc, &request).await {
                        Ok(response) => return download_bytes(response),
                        Err(InvocationError::Rpc(err)) if err.name == "AUTH_KEY_UNREGISTERED" => {
                            client.copy_auth_to_dc(task_dc).await?;
                        }
                        Err(InvocationError::Rpc(err)) if err.code == FILE_MIGRATE_ERROR => {
                            task_dc = migration_dc(err.value)?;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        })
        .await
    }

    /// Uploads an async stream to Telegram servers.
    ///
    /// The file is not sent to any peer, but can be used as media when sending messages for a
    /// certain period of time (less than a day). You can use this uploaded file multiple times.
    ///
    /// Refer to [`InputMessage`] to learn more uses for `uploaded_file`.
    ///
    /// The stream size must be known beforehand. If this is not possible, you might need to
    /// process the entire async stream to determine its size, and then use the size and the
    /// downloaded buffer.
    ///
    /// The stream size may be less or equal to the actual length of the stream, but not more.
    /// If it's less, you may continue to read from the stream after the method returns.
    /// If it's more, the method will fail because it does not have enough data to read.
    ///
    /// Note that Telegram uses the file name in certain methods, for example, to make sure the
    /// file is an image when trying to use send the file as photo media, so it is important that
    /// the file name at least uses the right extension, even if the name is a dummy value.
    /// If the input file name is empty, the non-empty dummy value "a" will be used instead.
    /// Because it has no extension, you may not be able to use the file in certain methods.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client, some_vec: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    /// use grammers_client::message::InputMessage;
    ///
    /// // In-memory `Vec<u8>` buffers can be used as async streams
    /// let size = some_vec.len();
    /// let mut stream = std::io::Cursor::new(some_vec);
    /// let uploaded_file = client.upload_stream(&mut stream, size, "sleep.jpg".to_string()).await?;
    ///
    /// client.send_message(peer, InputMessage::new().text("Zzz...").photo(uploaded_file)).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`InputMessage`]: crate::message::InputMessage
    pub async fn upload_stream<S: AsyncRead + Unpin>(
        &self,
        stream: &mut S,
        size: usize,
        name: String,
    ) -> Result<Uploaded, io::Error> {
        let file_id = generate_random_id();
        let name = if name.is_empty() {
            "a".to_string()
        } else {
            name
        };

        let big_file = size > BIG_FILE_SIZE;
        let parts = PartStream::new(stream, size);
        let total_parts = parts.total_parts();

        if big_file {
            let parts = Arc::new(parts);
            let mut tasks = FuturesUnordered::new();
            for _ in 0..WORKER_COUNT {
                let handle = self.clone();
                let parts = Arc::clone(&parts);
                let task = async move {
                    while let Some((part, bytes)) = parts.next_part().await? {
                        let ok = handle
                            .invoke(&tl::functions::upload::SaveBigFilePart {
                                file_id,
                                file_part: part,
                                file_total_parts: total_parts,
                                bytes,
                            })
                            .await
                            .map_err(io::Error::other)?;

                        if !ok {
                            return Err(io::Error::other("server failed to store uploaded data"));
                        }
                    }
                    Ok(())
                };
                tasks.push(task);
            }

            while let Some(res) = tasks.next().await {
                res?;
            }

            Ok(Uploaded::from_raw(
                tl::types::InputFileBig {
                    id: file_id,
                    parts: total_parts,
                    name,
                }
                .into(),
            ))
        } else {
            let mut md5 = md5::Context::new();
            while let Some((part, bytes)) = parts.next_part().await? {
                md5.consume(&bytes);
                let ok = self
                    .invoke(&tl::functions::upload::SaveFilePart {
                        file_id,
                        file_part: part,
                        bytes,
                    })
                    .await
                    .map_err(io::Error::other)?;

                if !ok {
                    return Err(io::Error::other("server failed to store uploaded data"));
                }
            }
            Ok(Uploaded::from_raw(
                tl::types::InputFile {
                    id: file_id,
                    parts: total_parts,
                    name,
                    md5_checksum: format!("{:x}", md5.finalize()),
                }
                .into(),
            ))
        }
    }

    /// Uploads a local file to Telegram servers.
    ///
    /// The file is not sent to any peer, but can be used as media when sending messages for a
    /// certain period of time (less than a day). You can use this uploaded file multiple times.
    ///
    /// Refer to [`InputMessage`] to learn more uses for `uploaded_file`.
    ///
    /// If you need more control over the uploaded data, such as performing only a partial upload
    /// or with a different name, use [`Client::upload_stream`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// use grammers_client::message::InputMessage;
    ///
    /// let uploaded_file = client.upload_file("/home/username/photos/holidays.jpg").await?;
    ///
    /// client.send_message(peer, InputMessage::new().text("Check this out!").photo(uploaded_file)).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`InputMessage`]: crate::message::InputMessage
    #[cfg(feature = "fs")]
    pub async fn upload_file<P: AsRef<Path>>(&self, path: P) -> Result<Uploaded, io::Error> {
        let path = path.as_ref();

        let mut file = fs::File::open(path).await?;
        let size = file.seek(SeekFrom::End(0)).await? as usize;
        file.seek(SeekFrom::Start(0)).await?;

        // File name will only be `None` for `..` path, and directories cannot be uploaded as
        // files, so it's fine to unwrap.
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        self.upload_stream(&mut file, size, name).await
    }
}

struct PartStreamInner<'a, S: AsyncRead + Unpin> {
    stream: &'a mut S,
    current_part: i32,
}

struct PartStream<'a, S: AsyncRead + Unpin> {
    inner: AsyncMutex<PartStreamInner<'a, S>>,
    total_parts: i32,
}

impl<'a, S: AsyncRead + Unpin> PartStream<'a, S> {
    fn new(stream: &'a mut S, size: usize) -> Self {
        let total_parts = size.div_ceil(MAX_CHUNK_SIZE as usize) as i32;
        Self {
            inner: AsyncMutex::new(PartStreamInner {
                stream,
                current_part: 0,
            }),
            total_parts,
        }
    }

    fn total_parts(&self) -> i32 {
        self.total_parts
    }

    async fn next_part(&self) -> Result<Option<(i32, Vec<u8>)>, io::Error> {
        let mut lock = self.inner.lock().await;
        if lock.current_part >= self.total_parts {
            return Ok(None);
        }
        let mut read = 0;
        let mut buffer = vec![0; MAX_CHUNK_SIZE as usize];

        while read != buffer.len() {
            let n = lock.stream.read(&mut buffer[read..]).await?;
            if n == 0 {
                if lock.current_part == self.total_parts - 1 {
                    break;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "reached EOF before reaching the last file part",
                    ));
                }
            }
            read += n;
        }

        let bytes = if read == buffer.len() {
            buffer
        } else {
            buffer[..read].to_vec()
        };

        let res = Ok(Some((lock.current_part, bytes)));
        lock.current_part += 1;
        res
    }
}

#[cfg(all(test, feature = "fs"))]
mod safety_tests {
    use super::*;
    use grammers_mtsender::SenderPool;
    use grammers_session::storages::MemorySession;

    fn location() -> tl::enums::InputFileLocation {
        tl::types::InputDocumentFileLocation {
            id: 1,
            access_hash: 0,
            file_reference: Vec::new(),
            thumb_size: String::new(),
        }
        .into()
    }

    #[tokio::test]
    async fn zero_download_workers_cannot_report_success_for_unwritten_file() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 12345);
        let client = Client::new(pool.handle);
        let path =
            std::env::temp_dir().join(format!("grammers-zero-workers-{}", generate_random_id()));
        let result = client
            .download_media_concurrent(location(), 4, &path, 0, 2)
            .await;
        let _ = fs::remove_file(path).await;
        assert!(
            matches!(result, Err(InvocationError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput)
        );
    }

    #[tokio::test]
    async fn cancelling_parallel_download_drops_all_worker_clients() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 12345);
        // Keep the runner alive but never run it: each worker remains pending on its RPC.
        let client = Client::new(pool.handle);
        let path =
            std::env::temp_dir().join(format!("grammers-cancel-download-{}", generate_random_id()));
        let task_client = client.clone();
        let task_path = path.clone();
        let task = tokio::spawn(async move {
            task_client
                .download_media_concurrent(
                    location(),
                    MAX_CHUNK_SIZE as usize * 8,
                    task_path,
                    WORKER_COUNT,
                    2,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&client.0) < WORKER_COUNT + 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all download workers started");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let count = Arc::strong_count(&client.0);
        drop(pool.runner);
        let _ = fs::remove_file(path).await;
        assert_eq!(
            count, 1,
            "download cancellation must drop every worker-owned client"
        );
    }
    #[test]
    fn cdn_redirect_returns_unsupported_without_exposing_key_material() {
        let response = tl::types::upload::FileCdnRedirect {
            dc_id: 2,
            file_token: b"secret-token".to_vec(),
            encryption_key: vec![7; 32],
            encryption_iv: vec![8; 16],
            file_hashes: Vec::new(),
        }
        .into();
        let Err(InvocationError::Io(error)) = download_bytes(response) else {
            panic!("expected unsupported CDN error")
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("CDN"));
        assert!(!format!("{error:?}").contains("secret-token"));
    }

    #[tokio::test]
    async fn chunk_results_preserve_offsets_and_exact_final_size() {
        let chunk_size = MAX_CHUNK_SIZE as usize;
        let size = chunk_size * 5 + 3;
        let mut file = std::io::Cursor::new(Vec::new());
        download_chunks(&mut file, size, WORKER_COUNT, |offset| async move {
            if offset == 0 {
                tokio::task::yield_now().await;
            }
            Ok(vec![
                (offset as usize / chunk_size + 1) as u8;
                (size - offset as usize).min(chunk_size)
            ])
        })
        .await
        .unwrap();
        let bytes = file.into_inner();
        assert_eq!(bytes.len(), size);
        for (index, chunk) in bytes.chunks(chunk_size).enumerate() {
            assert!(chunk.iter().all(|byte| *byte == index as u8 + 1));
        }
    }

    #[tokio::test]
    async fn short_or_oversized_chunks_cannot_leave_silent_gaps() {
        for (len, kind) in [
            (3, io::ErrorKind::UnexpectedEof),
            (5, io::ErrorKind::InvalidData),
        ] {
            let mut file = std::io::Cursor::new(Vec::new());
            let result =
                download_chunks(
                    &mut file,
                    4,
                    WORKER_COUNT,
                    |_| async move { Ok(vec![1; len]) },
                )
                .await;
            assert!(matches!(result, Err(InvocationError::Io(error)) if error.kind() == kind));
            assert!(file.into_inner().is_empty());
        }
    }

    struct ActiveRequest(Arc<std::sync::atomic::AtomicUsize>);
    impl ActiveRequest {
        fn new(count: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Self(count.clone())
        }
    }
    impl Drop for ActiveRequest {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn worker_failure_drops_other_pending_requests_and_preserves_error() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut file = std::io::Cursor::new(Vec::new());
        let result = download_chunks(
            &mut file,
            MAX_CHUNK_SIZE as usize * 8,
            WORKER_COUNT,
            |offset| {
                let guard = ActiveRequest::new(&count);
                async move {
                    let _guard = guard;
                    if offset == 0 {
                        return Err(InvocationError::Dropped("fixture request failed"));
                    }
                    std::future::pending::<Result<Vec<u8>, InvocationError>>().await
                }
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(InvocationError::Dropped("fixture request failed"))
        ));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct FailingWriter {
        fail_on_flush: bool,
    }
    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            if self.fail_on_flush {
                return std::task::Poll::Ready(Ok(bytes.len()));
            }
            std::task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture disk error",
            )))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if self.fail_on_flush {
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture flush failure",
                )));
            }
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    impl AsyncSeek for FailingWriter {
        fn start_seek(self: std::pin::Pin<&mut Self>, _: SeekFrom) -> io::Result<()> {
            Ok(())
        }
        fn poll_complete(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<u64>> {
            std::task::Poll::Ready(Ok(0))
        }
    }

    #[tokio::test]
    async fn disk_failure_drops_pending_requests_without_result_channel_panics() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = download_chunks(
            &mut FailingWriter {
                fail_on_flush: false,
            },
            MAX_CHUNK_SIZE as usize * 8,
            WORKER_COUNT,
            |offset| {
                let guard = ActiveRequest::new(&count);
                async move {
                    let _guard = guard;
                    if offset == 0 {
                        return Ok(vec![1; MAX_CHUNK_SIZE as usize]);
                    }
                    std::future::pending::<Result<Vec<u8>, InvocationError>>().await
                }
            },
        )
        .await;
        assert!(
            matches!(result, Err(InvocationError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pending_chunk_requests_are_bounded() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let high_water = std::sync::atomic::AtomicUsize::new(0);
        let mut file = std::io::Cursor::new(Vec::new());
        download_chunks(&mut file, MAX_CHUNK_SIZE as usize * 8, WORKER_COUNT, |_| {
            let guard = ActiveRequest::new(&count);
            high_water.fetch_max(
                count.load(std::sync::atomic::Ordering::SeqCst),
                std::sync::atomic::Ordering::SeqCst,
            );
            async move {
                let _guard = guard;
                Ok(vec![1; MAX_CHUNK_SIZE as usize])
            }
        })
        .await
        .unwrap();
        assert_eq!(
            high_water.load(std::sync::atomic::Ordering::SeqCst),
            WORKER_COUNT
        );
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn final_flush_failure_is_returned_to_the_caller() {
        let result = download_chunks(
            &mut FailingWriter {
                fail_on_flush: true,
            },
            4,
            WORKER_COUNT,
            |_| async { Ok(vec![1; 4]) },
        )
        .await;
        assert!(
            matches!(result, Err(InvocationError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied)
        );
    }

    #[tokio::test]
    async fn sequential_download_preserves_structured_error_kind() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 12345);
        let client = Client::new(pool.handle);
        let mut download = DownloadIter {
            client,
            done: false,
            size: None,
            variant: DownloadIterVariant::PreFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixture unsupported download",
            )),
            dc_id: None,
        };
        let path =
            std::env::temp_dir().join(format!("grammers-download-error-{}", generate_random_id()));
        let result = Client::load(&path, &mut download).await;
        let _ = fs::remove_file(path).await;
        assert!(
            matches!(result, Err(InvocationError::Io(error)) if error.kind() == io::ErrorKind::Unsupported)
        );
    }
}
