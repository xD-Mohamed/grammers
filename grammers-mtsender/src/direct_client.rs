// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Direct client implementation like gotd/td.
//!
//! Unlike the regular Client which uses SenderPoolRunner (background task),
//! this client works directly with connections, minimizing file descriptors.
//! This is ideal for running thousands of clients simultaneously.

use std::sync::Arc;

use grammers_session::Session;
use grammers_tl_types as tl;

use crate::dc_pool::{DcConnectionPool, PoolConfig};
use crate::ConnectionParams;
use crate::errors::InvocationError;

/// Direct client that works directly with connections, like gotd/td.
///
/// Unlike the regular Client, this:
/// - Does NOT spawn background tasks
/// - Does NOT use channels
/// - Invokes directly on connections
///
/// This means:
/// - 1 Client = 1 Connection (when active)
/// - Minimal file descriptor overhead
/// - Perfect for thousands of clients
///
/// # Example
///
/// ```rust,no_run
/// use grammers_mtsender::DirectClient;
/// use grammers_session::MemorySession;
///
/// let session = Arc::new(MemorySession::new());
/// let client = DirectClient::connect(session, api_id, params).await?;
///
/// // Use the client
/// let result = client.invoke(&tl::functions::users::GetUsers {
///     id: vec![tl::enums::InputUser::UserSelf],
/// }).await?;
/// ```
pub struct DirectClient {
    session: Arc<dyn Session>,
    api_id: i32,
    connection_params: ConnectionParams,
    // Direct connection pool - no background tasks
    pool: DcConnectionPool,
}

impl DirectClient {
    /// Connect to Telegram and initialize the session.
    ///
    /// This will establish a connection to the primary DC and perform initial handshake.
    pub async fn connect(
        session: Arc<dyn Session>,
        api_id: i32,
        connection_params: ConnectionParams,
    ) -> Result<Self, InvocationError> {
        // Initialize with DC pool (limits to 1 connection per DC by default)
        let pool = DcConnectionPool::new(
            session.clone(),
            api_id,
            connection_params.clone(),
            PoolConfig::default(),
        );

        // Establish initial connection to the primary DC
        let primary_dc = session.home_dc_id();

        // Force a connection by making a simple request
        let _ = pool
            .invoke(primary_dc, &tl::functions::help::GetConfig {})
            .await?;

        Ok(Self {
            session,
            api_id,
            connection_params,
            pool,
        })
    }

    /// Get the API ID.
    pub fn api_id(&self) -> i32 {
        self.api_id
    }

    /// Get the session.
    pub fn session(&self) -> Arc<dyn Session> {
        self.session.clone()
    }

    /// Invoke a TL request directly on the connection.
    ///
    /// This is the primary method for making requests.
    pub async fn invoke<R: tl::RemoteCall>(
        &self,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        let dc_id = self.session.home_dc_id();

        let result = self.pool.invoke(dc_id, request).await?;
        Ok(result)
    }

    /// Check if the user is authorized.
    pub async fn is_authorized(&self) -> bool {
        // Try to get user info
        match self
            .invoke(&tl::functions::users::GetUsers {
                id: vec![tl::enums::InputUser::UserSelf],
            })
            .await
        {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /// Get the current user.
    pub async fn get_me(&self) -> Result<tl::types::User, InvocationError> {
        let users = self
            .invoke(&tl::functions::users::GetUsers {
                id: vec![tl::enums::InputUser::UserSelf],
            })
            .await?;

        if let Some(tl::enums::User::User(user)) = users.first() {
            Ok(user.clone())
        } else {
            Err(InvocationError::Dropped)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}
