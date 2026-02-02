// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! DC-level connection pooling similar to gotd/td.
//!
//! This implements connection pooling like gotd/td:
//! - Connections are created on-demand when invoking
//! - Connections are reused across multiple invocations
//! - Each connection is wrapped in a Mutex for shared access
//! - Minimal file descriptor usage (one TCP socket per DC per pool)

use std::collections::HashMap;
use std::sync::Arc;

use grammers_mtproto::{mtp, transport};
use grammers_session::Session;
use grammers_session::types::DcOption;
use grammers_tl_types::Deserializable;
use grammers_tl_types as tl;
use tokio::sync::Mutex as TokioMutex;

use crate::errors::InvocationError;
use crate::sender::Sender;
use crate::{connect, connect_with_auth, ConnectionParams, ServerAddr};

/// Configuration for the DC connection pool, similar to gotd/td's `Config`.
#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// Maximum number of connections per datacenter (like gotd/td's `MaxOpenConnections`).
    ///
    /// This limits the total number of TCP connections to each DC.
    /// When the limit is reached, new requests will wait for an existing connection.
    ///
    /// Default is 1 to minimize total connections when using many sessions.
    pub max_connections_per_dc: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_dc: 1,
        }
    }
}

/// A pooled connection wrapped in a Mutex for shared access.
///
/// Unlike gotd/td which uses acquire/release with a free pool,
/// we use Mutex to serialize access to the connection.
/// This is simpler and works well for Rust's ownership model.
type PooledConnection = Arc<TokioMutex<Sender<transport::Full, mtp::Encrypted>>>;

/// DC-level connection pool, similar to gotd/td's `Pool`.
///
/// This manages connections to datacenters with a limit per DC.
///
/// # How it works
///
/// ```text
/// invoke(dc_id) → get/create connection for DC → invoke on connection
/// ```
///
/// Connections are stored in a HashMap and reused across invocations.
pub struct DcConnectionPool {
    session: Arc<dyn Session>,
    api_id: i32,
    connection_params: ConnectionParams,
    #[allow(dead_code)]
    config: PoolConfig,
    /// Map of DC ID to pooled connections.
    connections: TokioMutex<HashMap<i32, PooledConnection>>,
    /// Semaphore to limit concurrent connection creation.
    creating: TokioMutex<HashMap<i32, ()>>,
}

impl DcConnectionPool {
    /// Create a new DC connection pool.
    pub fn new(
        session: Arc<dyn Session>,
        api_id: i32,
        connection_params: ConnectionParams,
        config: PoolConfig,
    ) -> Self {
        Self {
            session,
            api_id,
            connection_params,
            config,
            connections: TokioMutex::new(HashMap::new()),
            creating: TokioMutex::new(HashMap::new()),
        }
    }

    /// Invoke a request.
    ///
    /// This implements a simplified version of gotd/td's `Invoke()`:
    /// 1. Get or create a connection for the DC
    /// 2. Lock the connection (Mutex)
    /// 3. Invoke the request
    /// 4. Connection is automatically released (Mutex unlocked)
    pub async fn invoke<R: tl::RemoteCall>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        let body = request.to_bytes();

        // Get or create connection for this DC
        let conn = self.get_or_create_connection(dc_id).await?;

        // Lock the connection and invoke
        let mut sender = conn.lock().await;
        let result = Self::do_invoke(&mut *sender, body).await?;

        R::Return::from_bytes(&result).map_err(|_e| InvocationError::Dropped)
    }

    /// Invoke with pre-serialized request body.
    pub(crate) async fn invoke_raw(
        &self,
        dc_id: i32,
        body: Vec<u8>,
        tx: tokio::sync::oneshot::Sender<Result<Vec<u8>, InvocationError>>,
    ) -> Result<(), InvocationError> {
        // Get or create connection for this DC
        let conn = self.get_or_create_connection(dc_id).await?;

        // Invoke directly (no extra task spawn to reduce overhead)
        let mut sender = conn.lock().await;
        let result = Self::do_invoke(&mut *sender, body).await;
        let _ = tx.send(result);

        Ok(())
    }

    /// Get or create a connection for the given DC.
    async fn get_or_create_connection(&self, dc_id: i32) -> Result<PooledConnection, InvocationError> {
        // First, check if we already have a connection
        {
            let conns = self.connections.lock().await;
            if let Some(conn) = conns.get(&dc_id) {
                return Ok(Arc::clone(conn));
            }
        }

        // Check if we're already creating a connection for this DC
        {
            let mut creating = self.creating.lock().await;
            if creating.contains_key(&dc_id) {
                // Wait for the connection to be created
                drop(creating);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                let conns = self.connections.lock().await;
                if let Some(conn) = conns.get(&dc_id) {
                    return Ok(Arc::clone(conn));
                }
                return Err(InvocationError::Dropped);
            }
            creating.insert(dc_id, ());
        }

        // Create a new connection
        let sender = self.create_connection(dc_id).await?;

        // Remove from creating map
        {
            let mut creating = self.creating.lock().await;
            creating.remove(&dc_id);
        }

        let conn = Arc::new(TokioMutex::new(sender));

        // Store the connection
        {
            let mut conns = self.connections.lock().await;
            conns.insert(dc_id, Arc::clone(&conn));
        }

        Ok(conn)
    }

    /// Create a new connection for the given DC.
    async fn create_connection(
        &self,
        dc_id: i32,
    ) -> Result<Sender<transport::Full, mtp::Encrypted>, InvocationError> {
        let Some(dc_option) = self.session.dc_option(dc_id) else {
            return Err(InvocationError::InvalidDc);
        };

        let transport = transport::Full::new;

        #[cfg(feature = "proxy")]
        let addr = || {
            if let Some(proxy) = self.connection_params.proxy_url.clone() {
                ServerAddr::Proxied {
                    address: dc_option.ipv4.into(),
                    proxy,
                }
            } else {
                ServerAddr::Tcp {
                    address: dc_option.ipv4.into(),
                }
            }
        };
        #[cfg(not(feature = "proxy"))]
        let addr = || ServerAddr::Tcp {
            address: dc_option.ipv4.into(),
        };

        let init_connection = tl::functions::InvokeWithLayer {
            layer: tl::LAYER,
            query: tl::functions::InitConnection {
                api_id: self.api_id,
                device_model: self.connection_params.device_model.clone(),
                system_version: self.connection_params.system_version.clone(),
                app_version: self.connection_params.app_version.clone(),
                system_lang_code: self.connection_params.system_lang_code.clone(),
                lang_pack: "".into(),
                lang_code: self.connection_params.lang_code.clone(),
                proxy: None,
                params: None,
                query: tl::functions::help::GetConfig {},
            },
        };

        let mut sender = if let Some(auth_key) = dc_option.auth_key {
            connect_with_auth(transport(), addr(), auth_key).await?
        } else {
            connect(transport(), addr()).await?
        };

        let tl::enums::Config::Config(remote_config) = match sender.invoke(&init_connection).await {
            Ok(config) => config,
            Err(InvocationError::Transport(transport::Error::BadStatus { status: 404 })) => {
                sender = connect(transport(), addr()).await?;
                sender.invoke(&init_connection).await?
            }
            Err(e) => return Err(e.into()),
        };

        // Update auth key in session
        let mut dc_option = dc_option;
        dc_option.auth_key = Some(sender.auth_key());
        self.session.set_dc_option(&dc_option).await;

        self.update_config(remote_config).await;

        Ok(sender)
    }

    async fn update_config(&self, config: tl::types::Config) {
        for option in config
            .dc_options
            .iter()
            .map(|tl::enums::DcOption::Option(option)| option)
            .filter(|option| !option.media_only && !option.tcpo_only && option.r#static)
        {
            let mut dc_option = self
                .session
                .dc_option(option.id)
                .unwrap_or_else(|| DcOption {
                    id: option.id,
                    ipv4: std::net::SocketAddrV4::new(std::net::Ipv4Addr::from_bits(0), 0),
                    ipv6: std::net::SocketAddrV6::new(std::net::Ipv6Addr::from_bits(0), 0, 0, 0),
                    auth_key: None,
                });
            if option.ipv6 {
                dc_option.ipv6 = std::net::SocketAddrV6::new(
                    option
                        .ip_address
                        .parse()
                        .expect("Telegram to return a valid IPv6 address"),
                    option.port as _,
                    0,
                    0,
                );
            } else {
                dc_option.ipv4 = std::net::SocketAddrV4::new(
                    option
                        .ip_address
                        .parse()
                        .expect("Telegram to return a valid IPv4 address"),
                    option.port as _,
                );
                if dc_option.ipv6.ip().to_bits() == 0 {
                    dc_option.ipv6 = std::net::SocketAddrV6::new(
                        dc_option.ipv4.ip().to_ipv6_mapped(),
                        dc_option.ipv4.port(),
                        0,
                        0,
                    )
                }
            }
        }
    }

    /// Perform the actual invocation on a sender.
    async fn do_invoke(
        sender: &mut Sender<transport::Full, mtp::Encrypted>,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, InvocationError> {
        // Create a channel for the response
        let (tx, mut rx) = tokio::sync::oneshot::channel();

        // Enqueue the request
        sender.enqueue_body(body, tx);

        // Process until we get a response
        loop {
            sender.step().await?;
            // Check if we got a response
            match rx.try_recv() {
                Ok(result) => return result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => continue,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    return Err(InvocationError::Dropped);
                }
            }
        }
    }

    /// Disconnect from a specific datacenter.
    pub async fn disconnect(&self, dc_id: i32) {
        let mut conns = self.connections.lock().await;
        conns.remove(&dc_id);
    }

    /// Shutdown the pool and close all connections.
    pub async fn shutdown(self) {
        let _ = self.connections.lock().await;
        // Connections will be dropped when the HashMap is dropped
        // The TCP sockets will be closed when the Senders are dropped
    }

    /// Clean up any dead connections (no-op in this implementation).
    ///
    /// In the new implementation, connections are stored in the HashMap
    /// and don't need manual cleanup like the old background-task-based approach.
    pub fn cleanup(&self) {
        // No-op: connections are managed via HashMap and dropped automatically
    }
}
