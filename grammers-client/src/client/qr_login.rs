// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Explicit QR-login steps, following <https://core.telegram.org/api/qr-login>.

use super::PasswordToken;
use crate::{Client, InvocationError, SignInError, peer::User, tl};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    fmt, io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// A short-lived QR credential. Only `url()` reveals its contents.
pub struct QrLoginToken {
    token: Vec<u8>,
    expires: SystemTime,
}
impl fmt::Debug for QrLoginToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("QrLoginToken([redacted])")
    }
}
impl QrLoginToken {
    fn new(token: Vec<u8>, expires: i32) -> Result<Self, InvocationError> {
        if token.is_empty() || expires <= 0 {
            return Err(invalid_response());
        }
        Ok(Self {
            token,
            expires: UNIX_EPOCH + Duration::from_secs(expires as u64),
        })
    }
    /// The sensitive `tg://login` URL to render as a QR code in a trusted UI.
    pub fn url(&self) -> String {
        format!("tg://login?token={}", URL_SAFE_NO_PAD.encode(&self.token))
    }
    /// Server expiry as a UTC system timestamp.
    pub fn expires_at(&self) -> SystemTime {
        self.expires
    }
    /// Whether this token needs refreshing at the supplied time.
    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        now >= self.expires
    }
}
/// One explicitly requested QR-login step; no polling or renderer is started.
pub enum QrLogin {
    /// Render the token and explicitly step again on expiry or `updateLoginToken`.
    Display(QrLoginToken),
    /// Login completed and session state was persisted through the usual login path.
    Authorized(Box<User>),
}

impl fmt::Debug for QrLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Display(token) => f.debug_tuple("Display").field(token).finish(),
            Self::Authorized(_) => f.write_str("Authorized([redacted])"),
        }
    }
}

fn invalid_response() -> InvocationError {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid QR login response; request a fresh login token",
    )
    .into()
}

trait QrTransport {
    async fn export(
        &mut self,
        api_hash: &str,
        except_ids: &[i64],
    ) -> Result<tl::enums::auth::LoginToken, InvocationError>;
    async fn import(
        &mut self,
        dc: i32,
        token: Vec<u8>,
    ) -> Result<tl::enums::auth::LoginToken, InvocationError>;
    async fn finish(
        &mut self,
        dc: Option<i32>,
        authorization: tl::types::auth::Authorization,
    ) -> Result<User, InvocationError>;
    async fn password(&mut self, dc: Option<i32>) -> Result<PasswordToken, InvocationError>;
}

async fn step(
    transport: &mut impl QrTransport,
    api_hash: &str,
    except_ids: &[i64],
) -> Result<QrLogin, SignInError> {
    let mut response = transport.export(api_hash, except_ids).await;
    let mut dc = None;
    let mut visited = Vec::new();
    loop {
        match response {
            Ok(tl::enums::auth::LoginToken::Token(token)) if dc.is_none() => {
                return QrLoginToken::new(token.token, token.expires)
                    .map(QrLogin::Display)
                    .map_err(SignInError::Other);
            }
            Ok(tl::enums::auth::LoginToken::Token(_)) => {
                return Err(SignInError::Other(invalid_response()));
            }
            Ok(tl::enums::auth::LoginToken::MigrateTo(token)) => {
                if token.dc_id <= 0 {
                    return Err(SignInError::Other(InvocationError::InvalidDc));
                }
                if token.token.is_empty() || visited.contains(&token.dc_id) || visited.len() >= 3 {
                    return Err(SignInError::Other(invalid_response()));
                }
                visited.push(token.dc_id);
                dc = Some(token.dc_id);
                response = transport.import(token.dc_id, token.token).await;
            }
            Ok(tl::enums::auth::LoginToken::Success(token)) => {
                return match token.authorization {
                    tl::enums::auth::Authorization::Authorization(auth) => transport
                        .finish(dc, auth)
                        .await
                        .map(|user| QrLogin::Authorized(Box::new(user)))
                        .map_err(SignInError::Other),
                    tl::enums::auth::Authorization::SignUpRequired(_) => {
                        Err(SignInError::SignUpRequired)
                    }
                };
            }
            Err(error) if error.is("SESSION_PASSWORD_NEEDED") => {
                return match transport.password(dc).await {
                    Ok(token) => Err(SignInError::PasswordRequired(token)),
                    Err(error) => Err(SignInError::Other(error)),
                };
            }
            Err(error) => return Err(SignInError::Other(error)),
        }
    }
}

struct ClientTransport<'a>(&'a Client);
impl ClientTransport<'_> {
    async fn select_home(&self, dc: Option<i32>) -> Result<(), InvocationError> {
        if let Some(dc) = dc {
            let old = self.0.0.session.home_dc_id()?;
            self.0.0.session.set_home_dc_id(dc).await?;
            if dc != old {
                self.0.0.handle.disconnect_from_dc(old);
            }
        }
        Ok(())
    }
}
impl QrTransport for ClientTransport<'_> {
    async fn export(
        &mut self,
        api_hash: &str,
        except_ids: &[i64],
    ) -> Result<tl::enums::auth::LoginToken, InvocationError> {
        self.0
            .invoke(&tl::functions::auth::ExportLoginToken {
                api_id: self.0.0.api_id,
                api_hash: api_hash.to_owned(),
                except_ids: except_ids.to_vec(),
            })
            .await
    }
    async fn import(
        &mut self,
        dc: i32,
        token: Vec<u8>,
    ) -> Result<tl::enums::auth::LoginToken, InvocationError> {
        self.0
            .invoke_in_dc(dc, &tl::functions::auth::ImportLoginToken { token })
            .await
    }
    async fn finish(
        &mut self,
        dc: Option<i32>,
        authorization: tl::types::auth::Authorization,
    ) -> Result<User, InvocationError> {
        self.select_home(dc).await?;
        self.0.complete_login(authorization).await
    }
    async fn password(&mut self, dc: Option<i32>) -> Result<PasswordToken, InvocationError> {
        let home = self.0.0.session.home_dc_id()?;
        let password = self
            .0
            .invoke_in_dc(dc.unwrap_or(home), &tl::functions::account::GetPassword {})
            .await?;
        // Select the new home only after the password challenge was retrieved there.
        // check_password uses that home for its normal SRP and persistence path.
        self.select_home(dc).await?;
        Ok(PasswordToken::new(password.into()))
    }
}

impl Client {
    /// Begin, refresh, or check a QR login with one explicit protocol step.
    ///
    /// Render [`QrLoginToken::url`] in a trusted QR UI. Call this again after the
    /// token expires or Telegram sends `updateLoginToken`. Event-driven waiting
    /// requires updates to be enabled; clients without updates can explicitly
    /// check under their own schedule. This method starts no task or polling loop.
    ///
    /// `except_ids` excludes already logged-in user IDs. A successful login uses
    /// the ordinary session persistence path. 2FA returns the existing
    /// [`SignInError::PasswordRequired`] token for [`Client::check_password`].
    /// Migration imports are finite, reject cycles, and change the home DC only
    /// after authorization or an established password challenge. The API hash
    /// is used for this call only. QR URLs are short-lived login credentials.
    pub async fn qr_login_step(
        &self,
        api_hash: &str,
        except_ids: &[i64],
    ) -> Result<QrLogin, SignInError> {
        step(&mut ClientTransport(self), api_hash, except_ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[tokio::test]
    async fn qr_step_future_can_be_supervised_on_send_executors() {
        fn assert_send(_: impl std::future::Future + Send) {}
        let client = Client::builder(1).build().await.unwrap();
        assert_send(client.qr_login_step("not-used-without-polling", &[]));
    }

    struct Fake {
        replies: VecDeque<Result<tl::enums::auth::LoginToken, InvocationError>>,
        events: Vec<String>,
        home: i32,
        fail_password: bool,
        fail_finish: bool,
    }
    impl Fake {
        fn new(replies: Vec<Result<tl::enums::auth::LoginToken, InvocationError>>) -> Self {
            Self {
                replies: replies.into(),
                events: Vec::new(),
                home: 2,
                fail_password: false,
                fail_finish: false,
            }
        }
    }
    impl QrTransport for Fake {
        async fn export(
            &mut self,
            api_hash: &str,
            except_ids: &[i64],
        ) -> Result<tl::enums::auth::LoginToken, InvocationError> {
            assert_eq!(api_hash, "private-hash");
            assert_eq!(except_ids, [42]);
            self.events.push("export".into());
            self.replies.pop_front().expect("unexpected export")
        }
        async fn import(
            &mut self,
            dc: i32,
            token: Vec<u8>,
        ) -> Result<tl::enums::auth::LoginToken, InvocationError> {
            assert_eq!(token, [7]);
            assert_eq!(self.home, 2, "home changed before import succeeds");
            self.events.push(format!("import:{dc}"));
            self.replies.pop_front().expect("unexpected import")
        }
        async fn finish(
            &mut self,
            dc: Option<i32>,
            authorization: tl::types::auth::Authorization,
        ) -> Result<User, InvocationError> {
            self.events.push(format!("finish:{dc:?}"));
            if self.fail_finish {
                return Err(InvocationError::InvalidDc);
            }
            if let Some(dc) = dc {
                self.home = dc;
            }
            let client = Client::builder(1).build().await.unwrap();
            Ok(User::from_raw(&client, authorization.user))
        }
        async fn password(&mut self, dc: Option<i32>) -> Result<PasswordToken, InvocationError> {
            self.events.push(format!("password:{dc:?}"));
            if self.fail_password {
                return Err(InvocationError::InvalidDc);
            }
            if let Some(dc) = dc {
                self.home = dc;
            }
            Ok(PasswordToken::new(tl::types::account::Password {
                has_recovery: false,
                has_secure_values: false,
                has_password: true,
                current_algo: None,
                srp_b: None,
                srp_id: None,
                hint: None,
                email_unconfirmed_pattern: None,
                new_algo: tl::enums::PasswordKdfAlgo::Unknown,
                new_secure_algo: tl::enums::SecurePasswordKdfAlgo::Unknown,
                secure_random: vec![1],
                pending_reset_date: None,
                login_email_pattern: None,
            }))
        }
    }
    fn success() -> tl::enums::auth::LoginToken {
        tl::types::auth::LoginTokenSuccess {
            authorization: tl::types::auth::Authorization {
                setup_password_required: false,
                otherwise_relogin_days: None,
                tmp_sessions: None,
                future_auth_token: None,
                user: tl::types::UserEmpty { id: 42 }.into(),
            }
            .into(),
        }
        .into()
    }
    fn migrate(dc_id: i32) -> tl::enums::auth::LoginToken {
        tl::types::auth::LoginTokenMigrateTo {
            dc_id,
            token: vec![7],
        }
        .into()
    }
    #[tokio::test]
    async fn success_is_finished_and_migration_import_precedes_home_change() {
        for migrated in [false, true] {
            let mut fake = Fake::new(if migrated {
                vec![Ok(migrate(4)), Ok(success())]
            } else {
                vec![Ok(success())]
            });
            let result = step(&mut fake, "private-hash", &[42]).await.unwrap();
            assert!(
                matches!(result, QrLogin::Authorized(user) if user.id().bare_id_unchecked() == 42)
            );
            if migrated {
                assert_eq!(fake.events, ["export", "import:4", "finish:Some(4)"]);
                assert_eq!(fake.home, 4);
            } else {
                assert_eq!(fake.events, ["export", "finish:None"]);
                assert_eq!(fake.home, 2);
            }
        }
    }
    fn password_needed() -> InvocationError {
        InvocationError::Rpc(crate::sender::RpcError::from(tl::types::RpcError {
            error_code: 401,
            error_message: "SESSION_PASSWORD_NEEDED".into(),
        }))
    }
    #[tokio::test]
    async fn unaccepted_tokens_are_displayed_without_changing_home_or_finishing_login() {
        let mut fake = Fake::new(vec![Ok(tl::types::auth::LoginToken {
            token: vec![251, 255],
            expires: 30,
        }
        .into())]);
        let QrLogin::Display(token) = step(&mut fake, "private-hash", &[42]).await.unwrap() else {
            panic!("expected token")
        };
        assert_eq!(token.url(), "tg://login?token=-_8");
        assert_eq!(fake.events, ["export"]);
        assert_eq!(fake.home, 2);
    }
    #[tokio::test]
    async fn failed_import_does_not_change_home_or_finish_login() {
        let mut fake = Fake::new(vec![Ok(migrate(4)), Err(InvocationError::InvalidDc)]);
        assert!(matches!(
            step(&mut fake, "private-hash", &[42]).await,
            Err(SignInError::Other(InvocationError::InvalidDc))
        ));
        assert_eq!(fake.events, ["export", "import:4"]);
        assert_eq!(fake.home, 2);
    }
    #[tokio::test]
    async fn password_challenge_is_requested_from_migrated_dc_and_errors_propagate() {
        for fail in [false, true] {
            let mut fake = Fake::new(vec![Ok(migrate(4)), Err(password_needed())]);
            fake.fail_password = fail;
            let result = step(&mut fake, "private-hash", &[42]).await;
            if fail {
                assert!(matches!(
                    result,
                    Err(SignInError::Other(InvocationError::InvalidDc))
                ));
                assert_eq!(fake.home, 2);
            } else {
                assert!(matches!(result, Err(SignInError::PasswordRequired(_))));
                assert_eq!(fake.home, 4);
            }
            assert_eq!(fake.events, ["export", "import:4", "password:Some(4)"]);
        }
    }
    #[tokio::test]
    async fn invalid_and_cyclic_migrations_never_finish_login() {
        for replies in [
            vec![Ok(migrate(0))],
            vec![Ok(migrate(-1))],
            vec![Ok(migrate(4)), Ok(migrate(4))],
            vec![
                Ok(migrate(3)),
                Ok(migrate(4)),
                Ok(migrate(5)),
                Ok(migrate(6)),
            ],
            vec![Ok(tl::types::auth::LoginTokenMigrateTo {
                dc_id: 4,
                token: vec![],
            }
            .into())],
            vec![
                Ok(migrate(4)),
                Ok(tl::types::auth::LoginToken {
                    token: vec![1],
                    expires: 30,
                }
                .into()),
            ],
        ] {
            let mut fake = Fake::new(replies);
            assert!(step(&mut fake, "private-hash", &[42]).await.is_err());
            assert_eq!(fake.home, 2);
            assert!(fake.events.len() <= 4);
            assert!(
                fake.events
                    .iter()
                    .all(|event| event == "export" || event.starts_with("import:"))
            );
        }
    }
    #[tokio::test]
    async fn sign_up_and_finish_errors_do_not_become_authorized() {
        let mut fake = Fake::new(vec![Ok(tl::types::auth::LoginTokenSuccess {
            authorization: tl::types::auth::AuthorizationSignUpRequired {
                terms_of_service: None,
            }
            .into(),
        }
        .into())]);
        assert!(matches!(
            step(&mut fake, "private-hash", &[42]).await,
            Err(SignInError::SignUpRequired)
        ));
        assert_eq!(fake.events, ["export"]);
        let mut fake = Fake::new(vec![Ok(success())]);
        fake.fail_finish = true;
        assert!(matches!(
            step(&mut fake, "private-hash", &[42]).await,
            Err(SignInError::Other(InvocationError::InvalidDc))
        ));
    }
    #[test]
    fn token_url_uses_base64url_without_padding_and_debug_is_redacted() {
        let token = QrLoginToken::new(vec![251, 255], 30).unwrap();
        assert_eq!(token.url(), "tg://login?token=-_8");
        assert!(!format!("{token:?}").contains("-_8"));
        assert!(!token.is_expired_at(UNIX_EPOCH + Duration::from_secs(29)));
        assert!(token.is_expired_at(UNIX_EPOCH + Duration::from_secs(30)));
        assert_eq!(token.expires_at(), UNIX_EPOCH + Duration::from_secs(30));
    }
    #[test]
    fn malformed_display_tokens_are_rejected_without_exposing_bytes() {
        for (token, expires) in [(vec![], 30), (vec![1], 0), (vec![1], -1)] {
            assert!(QrLoginToken::new(token, expires).is_err());
        }
    }
}
