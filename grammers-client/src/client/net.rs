// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::Arc;

use grammers_mtsender::{InvocationError, SenderPoolFatHandle};
use grammers_tl_types as tl;
use tokio::sync::Mutex;

use super::{Client, ClientConfiguration, ClientInner};

/// Method implementations directly related with network connectivity.
impl Client {
    /// Creates a client attached to an externally managed sender pool.
    /// No connection or account authorization is performed here. The caller must
    /// run and shut down the pool separately; prefer [`Client::builder`] for an
    /// owned lifecycle with [`Client::start`] and [`Client::stop`].
    ///
    /// The [`grammers_mtsender::SenderPoolHandle`] does not keep a reference to the [`grammers_session::Session`]
    /// or `api_id`, but the [`grammers_mtsender::SenderPool`] itself does, so the latter is used as input to guarantee
    /// that the values are correctly shared between the pool and the client handles.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use grammers_client::Client;
    /// use grammers_session::storages::MemorySession; // avoid this storage outside tests!
    /// use grammers_mtsender::SenderPool;
    ///
    /// // Note: these are example values and are not actually valid.
    /// //       Obtain your own with the developer's phone at https://my.telegram.org.
    /// const API_ID: i32 = 932939;
    ///
    /// # async fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let session = Arc::new(MemorySession::default());
    /// let pool = SenderPool::new(Arc::clone(&session), API_ID);
    /// let client = Client::new(pool.handle);
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(sender_pool: SenderPoolFatHandle) -> Self {
        Self::with_configuration(sender_pool, Default::default())
    }

    /// Like [`Self::new`] but with a custom [`ClientConfiguration`].
    pub fn with_configuration(
        sender_pool: SenderPoolFatHandle,
        configuration: ClientConfiguration,
    ) -> Self {
        Self(Arc::new(ClientInner {
            session: sender_pool.session.clone(),
            api_id: sender_pool.api_id,
            handle: sender_pool.thin.clone(),
            auth_copied_to_dcs: Mutex::new(Vec::new()),
            stored_peers: Default::default(),
            configuration,
            lifecycle: None,
            #[cfg(test)]
            rpc_responder: None,
        }))
    }

    /// Invoke a raw API call. This directly sends the request to Telegram's servers.
    ///
    /// Using function definitions corresponding to a different layer is likely to cause the
    /// responses to the request to not be understood.
    ///
    /// <div class="warning">
    ///
    /// This method is **not** part of the stability guarantees of semantic
    /// versioning. It **may** break during *minor* version changes (but not on patch version
    /// changes). Use with care.
    ///
    /// </div>
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// use grammers_tl_types as tl;
    ///
    /// dbg!(client.invoke(&tl::functions::Ping { ping_id: 0 }).await?);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn invoke<R: tl::RemoteCall>(
        &self,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        let dc_id = self.0.session.home_dc_id()?;
        self.invoke_in_dc(dc_id, request).await
    }

    /// Like [`Self::invoke`], but in the specified DC.
    ///
    /// <div class="warning">
    ///
    /// This method is **not** part of the stability guarantees of semantic
    /// versioning. It **may** break during *minor* version changes (but not on patch version
    /// changes). Use with care.
    ///
    /// </div>
    pub async fn invoke_in_dc<R: tl::RemoteCall>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        #[cfg(test)]
        if let Some(responder) = &self.0.rpc_responder {
            let response = responder(dc_id, &tl::Serializable::to_bytes(request))?;
            return <R::Return as tl::Deserializable>::from_bytes(&response)
                .map_err(InvocationError::from);
        }
        self.0.handle.invoke_in_dc(dc_id, request).await
    }

    pub(crate) async fn copy_auth_to_dc(&self, target_dc_id: i32) -> Result<(), InvocationError> {
        let mut auth_copied_to_dcs = self.0.auth_copied_to_dcs.lock().await;
        let home_dc_id = self.0.session.home_dc_id()?;
        if target_dc_id == home_dc_id {
            return Ok(());
        }

        let mut target_key = self
            .0
            .session
            .dc_option(target_dc_id)?
            .and_then(|dc| dc.auth_key);
        if target_key.is_none() {
            // Authorization belongs to a key, so a missing key voids any earlier copy.
            auth_copied_to_dcs.retain(|(dc, _)| *dc != target_dc_id);
            // Create the key first with a call needing no authorization, so that a
            // successful import below can be attributed to a key known beforehand.
            self.invoke_in_dc(target_dc_id, &tl::functions::help::GetNearestDc {})
                .await?;
            target_key = self
                .0
                .session
                .dc_option(target_dc_id)?
                .and_then(|dc| dc.auth_key);
        }
        if target_key.as_ref().is_some_and(|key| {
            auth_copied_to_dcs
                .iter()
                .any(|(dc, copied_key)| *dc == target_dc_id && copied_key.as_ref() == key)
        }) {
            return Ok(());
        }
        // Authorization belongs to a key, not a DC number.
        auth_copied_to_dcs.retain(|(dc, _)| *dc != target_dc_id);

        let tl::enums::auth::ExportedAuthorization::Authorization(exported_auth) = self
            .invoke(&tl::functions::auth::ExportAuthorization {
                dc_id: target_dc_id,
            })
            .await?;

        self.invoke_in_dc(
            target_dc_id,
            &tl::functions::auth::ImportAuthorization {
                id: exported_auth.id,
                bytes: exported_auth.bytes,
            },
        )
        .await?;

        // Setup may replace the key during import. Never attribute success to a
        // newly observed key; the mutex coalesces imports for an unchanged key.
        if let Some(key) = target_key {
            let current_key = self
                .0
                .session
                .dc_option(target_dc_id)?
                .and_then(|dc| dc.auth_key);
            if current_key.as_ref() == Some(&key) {
                auth_copied_to_dcs.push((target_dc_id, Box::new(key)));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
pub(super) fn test_self_user(id: i64, hash: Option<i64>, bot: bool) -> tl::enums::User {
    use tl::{Deserializable, Identifiable};
    let flags = (1_u32 << 10) | u32::from(hash.is_some()) | (u32::from(bot) << 14);
    let mut bytes = tl::types::User::CONSTRUCTOR_ID.to_le_bytes().to_vec();
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&id.to_le_bytes());
    if let Some(hash) = hash {
        bytes.extend_from_slice(&hash.to_le_bytes());
    }
    if bot {
        bytes.extend_from_slice(&0_i32.to_le_bytes());
    }
    tl::enums::User::from_bytes(&bytes).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_mtsender::SenderPool;
    use grammers_session::Session;
    use grammers_session::storages::MemorySession;
    #[tokio::test]
    async fn authorization_copy_cache_does_not_survive_key_replacement() {
        for replacement in [Some([2; 256]), None] {
            let session = Arc::new(MemorySession::default());
            session.set_home_dc_id(2).await.unwrap();
            let target_dc = 4;
            let mut dc = session.dc_option(target_dc).unwrap().unwrap();
            dc.auth_key = Some([1; 256]);
            session.set_dc_option(&dc).await.unwrap();
            let pool = SenderPool::new(Arc::clone(&session), 1);
            let client = Client::new(pool.handle);
            client
                .0
                .auth_copied_to_dcs
                .lock()
                .await
                .push((target_dc, Box::new([1; 256])));
            client.copy_auth_to_dc(target_dc).await.unwrap();
            dc.auth_key = replacement;
            session.set_dc_option(&dc).await.unwrap();
            tokio::select! {
                biased;
                result = client.copy_auth_to_dc(target_dc) => panic!("replacement key must export: {result:?}"),
                _ = std::future::ready(()) => {}
            }
            assert!(client.0.auth_copied_to_dcs.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn authorization_copy_without_a_key_establishes_it_before_importing() {
        use std::sync::Mutex;
        use tl::Identifiable;
        for fail_key_setup in [false, true] {
            let session = Arc::new(MemorySession::default());
            session.set_home_dc_id(2).await.unwrap();
            let target_dc = 4;
            let mut dc = session.dc_option(target_dc).unwrap().unwrap();
            dc.auth_key = None;
            session.set_dc_option(&dc).await.unwrap();
            let pool = SenderPool::new(Arc::clone(&session), 1);
            let mut client = Client::new(pool.handle);
            let calls: Arc<Mutex<Vec<(i32, u32)>>> = Arc::default();
            let responder_calls = Arc::clone(&calls);
            let responder_session = Arc::clone(&session);
            Arc::get_mut(&mut client.0).unwrap().rpc_responder =
                Some(Arc::new(move |dc_id, body: &[u8]| {
                    let id = u32::from_le_bytes(body[..4].try_into().unwrap());
                    responder_calls.lock().unwrap().push((dc_id, id));
                    use tl::Serializable;
                    Ok(match id {
                        tl::functions::help::GetNearestDc::CONSTRUCTOR_ID => {
                            if fail_key_setup {
                                return Err(InvocationError::Dropped("no connection"));
                            }
                            // What real connection setup persists for the target DC.
                            let mut dc = responder_session.dc_option(dc_id).unwrap().unwrap();
                            dc.auth_key = Some([9; 256]);
                            // In-memory storage completes without suspending.
                            let mut write = responder_session.set_dc_option(&dc);
                            let mut context =
                                std::task::Context::from_waker(std::task::Waker::noop());
                            assert!(write.as_mut().poll(&mut context).is_ready());
                            tl::enums::NearestDc::Dc(tl::types::NearestDc {
                                country: String::new(),
                                this_dc: dc_id,
                                nearest_dc: dc_id,
                            })
                            .to_bytes()
                        }
                        tl::functions::auth::ExportAuthorization::CONSTRUCTOR_ID => {
                            tl::enums::auth::ExportedAuthorization::Authorization(
                                tl::types::auth::ExportedAuthorization {
                                    id: 1,
                                    bytes: vec![1],
                                },
                            )
                            .to_bytes()
                        }
                        tl::functions::auth::ImportAuthorization::CONSTRUCTOR_ID => {
                            tl::enums::auth::Authorization::SignUpRequired(
                                tl::types::auth::AuthorizationSignUpRequired {
                                    terms_of_service: None,
                                },
                            )
                            .to_bytes()
                        }
                        other => panic!("unexpected request {other:08x}"),
                    })
                }));
            if fail_key_setup {
                // Nothing account-related happens before a usable key exists.
                assert!(client.copy_auth_to_dc(target_dc).await.is_err());
                assert_eq!(calls.lock().unwrap().len(), 1);
                continue;
            }
            client.copy_auth_to_dc(target_dc).await.unwrap();
            client.copy_auth_to_dc(target_dc).await.unwrap();
            assert_eq!(
                *calls.lock().unwrap(),
                [
                    (target_dc, tl::functions::help::GetNearestDc::CONSTRUCTOR_ID),
                    (2, tl::functions::auth::ExportAuthorization::CONSTRUCTOR_ID),
                    (
                        target_dc,
                        tl::functions::auth::ImportAuthorization::CONSTRUCTOR_ID
                    ),
                ],
                "exactly one import, and the second copy recognises its key"
            );
        }
    }
}
