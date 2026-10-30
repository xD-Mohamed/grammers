// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::Arc;
use std::{fmt, io};

use grammers_crypto::two_factor_auth::{calculate_2fa, check_p_and_g};
use grammers_mtsender::InvocationError;
use grammers_session::types::{PeerInfo, UpdateState, UpdatesState};
use grammers_tl_types as tl;

use super::Client;
use crate::peer::User;
use crate::utils;

/// The error type which is returned when signing in fails.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SignInError {
    /// Sign-up with an official client is required.
    ///
    /// Third-party applications, such as those developed with *grammers*,
    /// cannot be used to register new accounts. For more details, see this
    /// [comment regarding third-party app sign-ups](https://bugs.telegram.org/c/25410/1):
    /// > \[…] if a user doesn’t have a Telegram account yet,
    /// > they will need to create one first using an official mobile Telegram app.
    SignUpRequired,
    /// The account has 2FA enabled, and the password is required.
    PasswordRequired(PasswordToken),
    /// The code used to complete login was not valid.
    InvalidCode,
    /// The 2FA password used to complete login was not valid.
    InvalidPassword(PasswordToken),
    /// A generic invocation error occured.
    Other(InvocationError),
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SignInError::*;
        match self {
            SignUpRequired => write!(f, "sign in error: sign up with official client required"),
            PasswordRequired(_password) => write!(f, "2fa password required"),
            InvalidCode => write!(f, "sign in error: invalid code"),
            InvalidPassword(_password) => write!(f, "invalid password"),
            Other(e) => write!(f, "sign in error: {e}"),
        }
    }
}

impl std::error::Error for SignInError {}

/// Login token needed to continue the login process after sending the code.
pub struct LoginToken {
    pub(crate) phone: String,
    pub(crate) phone_code_hash: String,
}

/// Password token needed to complete a 2FA login.
pub struct PasswordToken {
    pub(crate) password: Box<tl::types::account::Password>,
}

impl fmt::Debug for PasswordToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordToken").finish_non_exhaustive()
    }
}

fn invalid_password_state() -> InvocationError {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "missing or invalid SRP password parameters; request fresh password information",
    )
    .into()
}

struct PasswordParameters<'a> {
    salt1: &'a [u8],
    salt2: &'a [u8],
    p: &'a [u8],
    g: &'a i32,
}

fn password_parameters(
    password: &tl::types::account::Password,
) -> Result<PasswordParameters<'_>, InvocationError> {
    let algo = password
        .current_algo
        .as_ref()
        .ok_or_else(invalid_password_state)?;
    if matches!(algo, tl::enums::PasswordKdfAlgo::Unknown) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported password algorithm; update the client",
        )
        .into());
    }
    let params = utils::extract_password_parameters(algo);
    let b = password.srp_b.as_ref().ok_or_else(invalid_password_state)?;
    if !password.has_password
        || password.srp_id.is_none()
        || !(2..=7).contains(params.3)
        || !(248..=256).contains(&b.len())
        || b.iter().all(|byte| *byte == 0)
        || password.secure_random.is_empty()
        || password.secure_random.len() > 256
        || password.secure_random.iter().all(|byte| *byte == 0)
    {
        return Err(invalid_password_state());
    }
    Ok(PasswordParameters {
        salt1: params.0,
        salt2: params.1,
        p: params.2,
        g: params.3,
    })
}

static SRP_JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
#[cfg(test)]
static SRP_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn run_srp_job<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, InvocationError> + Send + 'static,
) -> Result<T, InvocationError> {
    let permit = SRP_JOBS
        .acquire()
        .await
        .expect("SRP semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
    .map_err(|error| InvocationError::Io(io::Error::other(error)))?
}

async fn password_proof(
    info: Box<tl::types::account::Password>,
    password: Arc<[u8]>,
) -> Result<
    (
        Box<tl::types::account::Password>,
        Option<tl::functions::auth::CheckPassword>,
    ),
    InvocationError,
> {
    password_parameters(&info)?;
    run_srp_job(move || {
        let PasswordParameters { salt1, salt2, p, g } = password_parameters(&info)?;
        // Out-of-range B is unusable server parameter state, like bad p.
        // Returning no proof permits the existing single bounded refresh.
        let b = info.srp_b.as_deref().ok_or_else(invalid_password_state)?;
        if p.len() == 256 {
            let mut padded_b = [0; 256];
            padded_b[256 - b.len()..].copy_from_slice(b);
            if padded_b.as_slice() >= p {
                return Ok((info, None));
            }
        }
        if !check_p_and_g(p, g) {
            return Ok((info, None));
        }
        let (m1, g_a) = calculate_2fa(
            salt1,
            salt2,
            p,
            g,
            info.srp_b.clone().ok_or_else(invalid_password_state)?,
            info.secure_random.clone(),
            password,
        );
        let request = tl::functions::auth::CheckPassword {
            password: tl::enums::InputCheckPasswordSrp::Srp(tl::types::InputCheckPasswordSrp {
                srp_id: info.srp_id.ok_or_else(invalid_password_state)?,
                a: g_a.to_vec(),
                m1: m1.to_vec(),
            }),
        };
        Ok((info, Some(request)))
    })
    .await
}

fn migration_dc(value: Option<u32>) -> Result<i32, InvocationError> {
    value
        .and_then(|value| i32::try_from(value).ok())
        .filter(|dc| *dc > 0)
        .ok_or(InvocationError::InvalidDc)
}

fn login_code_response(
    response: tl::enums::auth::SentCode,
) -> Result<tl::types::auth::SentCode, InvocationError> {
    match response {
        tl::enums::auth::SentCode::Code(code) => Ok(code),
        tl::enums::auth::SentCode::Success(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "login code request already authorized the session; check authorization status",
        )
        .into()),
        tl::enums::auth::SentCode::PaymentRequired(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "login requires payment; complete login with an official Telegram client",
        )
        .into()),
    }
}

impl PasswordToken {
    pub fn new(password: tl::types::account::Password) -> Self {
        PasswordToken {
            password: Box::new(password),
        }
    }

    pub fn hint(&self) -> Option<&str> {
        self.password.hint.as_deref()
    }
}

/// Method implementations related with the authentication of the user into the API.
///
/// Most requests to the API require the user to have authorized their key, stored in the session,
/// before being able to use them.
impl Client {
    /// Returns `true` if the current account is authorized. Otherwise,
    /// logging in will be required before being able to invoke requests.
    ///
    /// This will likely be the first method you want to call on a connected [`Client`]. After you
    /// determine if the account is authorized or not, you will likely want to use either
    /// [`Client::bot_sign_in`] or [`Client::request_login_code`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// if client.is_authorized().await? {
    ///     println!("Client already authorized and ready to use!");
    /// } else {
    ///     println!("Client is not authorized, you will need to sign_in!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn is_authorized(&self) -> Result<bool, InvocationError> {
        match self.invoke(&tl::functions::updates::GetState {}).await {
            Ok(_) => Ok(true),
            Err(e) if matches!(e.root_cause(), InvocationError::Rpc(rpc) if rpc.code == 401) => {
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    pub(super) async fn complete_login(
        &self,
        auth: tl::types::auth::Authorization,
    ) -> Result<User, InvocationError> {
        // In the extremely rare case where `Err` happens, there's not much we can do.
        // `message_box` will try to correct its state as updates arrive.
        let update_state = self.invoke(&tl::functions::updates::GetState {}).await.ok();

        let user = User::from_raw(self, auth.user);
        let auth = user
            .to_ref()
            .await?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "authorization response contains no usable user reference",
                )
            })?
            .auth;

        self.0
            .session
            .cache_peer(PeerInfo::User {
                id: user.id().bare_id_unchecked(),
                auth: Some(auth),
                bot: Some(user.is_bot()),
                is_self: Some(true),
            })
            .await?;
        if let Some(tl::enums::updates::State::State(state)) = update_state {
            self.0
                .session
                .set_update_state(UpdateState::All(UpdatesState {
                    pts: state.pts,
                    qts: state.qts,
                    date: state.date,
                    seq: state.seq,
                    channels: Vec::new(),
                }))
                .await?;
        }

        Ok(user)
    }

    /// Signs in to the bot account associated with this token.
    ///
    /// This is the method you need to call to use the client under a bot account.
    ///
    /// It is recommended to save the session on successful login. Some session storages will do this
    /// automatically. If saving fails, it is recommended to [`Client::sign_out`]. If the session is never
    /// saved post-login, then the authorization will be "lost" in the list of logged-in clients, since it
    /// is unaccessible.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Note: these values are obviously fake.
    /// //       Obtain your own with the developer's phone at https://my.telegram.org.
    /// const API_HASH: &str = "514727c32270b9eb8cc16daf17e21e57";
    /// //       Obtain your own by talking to @BotFather via a Telegram app.
    /// const TOKEN: &str = "776609994:AAFXAy5-PawQlnYywUlZ_b_GOXgarR3ah_yq";
    ///
    /// let user = match client.bot_sign_in(TOKEN, API_HASH).await {
    ///     Ok(user) => user,
    ///     Err(err) => {
    ///         println!("Failed to sign in as a bot :(\n{}", err);
    ///         return Err(err.into());
    ///     }
    /// };
    ///
    /// if let Some(first_name) = user.first_name() {
    ///     println!("Signed in as {}!", first_name);
    /// } else {
    ///     println!("Signed in!");
    /// }
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bot_sign_in(&self, token: &str, api_hash: &str) -> Result<User, InvocationError> {
        let request = tl::functions::auth::ImportBotAuthorization {
            flags: 0,
            api_id: self.0.api_id,
            api_hash: api_hash.to_string(),
            bot_auth_token: token.to_string(),
        };

        let result = match self.invoke(&request).await {
            Ok(x) => x,
            Err(err) => {
                let new_dc_id = match err.root_cause() {
                    InvocationError::Rpc(rpc) if rpc.code == 303 => migration_dc(rpc.value)?,
                    _ => return Err(err),
                };
                let old_dc_id = self.0.session.home_dc_id()?;
                // Disconnect from current DC to cull the now-unused connection.
                // This also gives a chance for the new home DC to export its authorization
                // if there's a need to connect back to the old DC after having logged in.
                self.0.handle.disconnect_from_dc(old_dc_id);
                self.0.session.set_home_dc_id(new_dc_id).await?;
                self.invoke(&request).await?
            }
        };

        match result {
            tl::enums::auth::Authorization::Authorization(x) => self.complete_login(x).await,
            tl::enums::auth::Authorization::SignUpRequired(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bot authorization unexpectedly requires sign-up",
            )
            .into()),
        }
    }

    /// Requests the login code for the account associated to the given phone
    /// number via another Telegram application or SMS.
    ///
    /// This is the method you need to call before being able to sign in to a user account.
    /// After you obtain the code and it's inside your program (e.g. ask the user to enter it
    /// via the console's standard input), you will need to [`Client::sign_in`] to complete the
    /// process.
    ///
    /// Login flows requiring payment return [`InvocationError::Io`] with
    /// [`io::ErrorKind::Unsupported`]; complete those flows with an official Telegram client.
    /// If Telegram already authorized the session, an [`io::ErrorKind::InvalidData`] error
    /// directs the caller to check [`Client::is_authorized`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Note: these values are obviously fake.
    /// //       Obtain your own with the developer's phone at https://my.telegram.org.
    /// const API_HASH: &str = "514727c32270b9eb8cc16daf17e21e57";
    /// //       The phone used here does NOT need to be the same as the one used by the developer
    /// //       to obtain the API ID and hash.
    /// const PHONE: &str = "+1 415 555 0132";
    ///
    /// if !client.is_authorized().await? {
    ///     // We're not logged in, so request the login code.
    ///     client.request_login_code(PHONE, API_HASH).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn request_login_code(
        &self,
        phone: &str,
        api_hash: &str,
    ) -> Result<LoginToken, InvocationError> {
        let request = tl::functions::auth::SendCode {
            phone_number: phone.to_string(),
            api_id: self.0.api_id,
            api_hash: api_hash.to_string(),
            settings: tl::types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
                unknown_number: false,
            }
            .into(),
        };

        let sent_code: tl::types::auth::SentCode = match self.invoke(&request).await {
            Ok(x) => login_code_response(x)?,
            Err(err) => {
                let new_dc_id = match err.root_cause() {
                    InvocationError::Rpc(rpc) if rpc.code == 303 => migration_dc(rpc.value)?,
                    _ => return Err(err),
                };
                let old_dc_id = self.0.session.home_dc_id()?;
                // Disconnect from current DC to cull the now-unused connection.
                // This also gives a chance for the new home DC to export its authorization
                // if there's a need to connect back to the old DC after having logged in.
                self.0.handle.disconnect_from_dc(old_dc_id);
                self.0.session.set_home_dc_id(new_dc_id).await?;
                login_code_response(self.invoke(&request).await?)?
            }
        };

        Ok(LoginToken {
            phone: phone.to_string(),
            phone_code_hash: sent_code.phone_code_hash,
        })
    }

    /// Signs in to the user account.
    ///
    /// You must call [`Client::request_login_code`] before using this method in order to obtain
    /// necessary login token, and also have asked the user for the login code.
    ///
    /// It is recommended to save the session on successful login. Some session storages will do this
    /// automatically. If saving fails, it is recommended to [`Client::sign_out`]. If the session is never
    /// saved post-login, then the authorization will be "lost" in the list of logged-in clients, since it
    /// is unaccessible.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grammers_client::SignInError;
    ///
    ///  async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// # const API_HASH: &str = "";
    /// # const PHONE: &str = "";
    /// fn ask_code_to_user() -> String {
    ///     unimplemented!()
    /// }
    ///
    /// let token = client.request_login_code(PHONE, API_HASH).await?;
    /// let code = ask_code_to_user();
    ///
    /// let user = match client.sign_in(&token, &code).await {
    ///     Ok(user) => user,
    ///     Err(SignInError::PasswordRequired(_token)) => panic!("Please provide a password"),
    ///     Err(SignInError::SignUpRequired) => panic!("Sign up required"),
    ///     Err(err) => {
    ///         println!("Failed to sign in as a user :(\n{}", err);
    ///         return Err(err.into());
    ///     }
    /// };
    ///
    /// if let Some(first_name) = user.first_name() {
    ///     println!("Signed in as {}!", first_name);
    /// } else {
    ///   println!("Signed in!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn sign_in(&self, token: &LoginToken, code: &str) -> Result<User, SignInError> {
        match self
            .invoke(&tl::functions::auth::SignIn {
                phone_number: token.phone.clone(),
                phone_code_hash: token.phone_code_hash.clone(),
                phone_code: Some(code.to_string()),
                email_verification: None,
            })
            .await
        {
            Ok(tl::enums::auth::Authorization::Authorization(x)) => {
                self.complete_login(x).await.map_err(SignInError::Other)
            }
            Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => {
                Err(SignInError::SignUpRequired)
            }
            Err(err) if err.is("SESSION_PASSWORD_NEEDED") => {
                let password_token = self.get_password_information().await;
                match password_token {
                    Ok(token) => Err(SignInError::PasswordRequired(token)),
                    Err(e) => Err(SignInError::Other(e)),
                }
            }
            Err(err) if err.is("PHONE_CODE_*") => Err(SignInError::InvalidCode),
            Err(error) => Err(SignInError::Other(error)),
        }
    }

    /// Extract information needed for the two-factor authentication
    /// It's called automatically when we get SESSION_PASSWORD_NEEDED error during sign in.
    async fn get_password_information(&self) -> Result<PasswordToken, InvocationError> {
        let request = tl::functions::account::GetPassword {};

        let password: tl::types::account::Password = self.invoke(&request).await?.into();

        Ok(PasswordToken::new(password))
    }

    /// Sign in using two-factor authentication (user password).
    ///
    /// [`PasswordToken`] can be obtained from [`SignInError::PasswordRequired`] error after the
    /// [`Client::sign_in`] method fails.
    /// Missing or invalid password state returns [`SignInError::Other`] containing
    /// [`InvocationError::Io`] with [`io::ErrorKind::InvalidData`]. Unsupported algorithms
    /// use [`io::ErrorKind::Unsupported`]. Invalid prime parameters are refreshed once before
    /// being rejected. Prime and generator checks remain mandatory before calculating a proof.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_client::SignInError;
    ///
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// # const API_HASH: &str = "";
    /// # const PHONE: &str = "";
    /// fn get_user_password(hint: &str) -> Vec<u8> {
    ///     unimplemented!()
    /// }
    ///
    /// # let token = client.request_login_code(PHONE, API_HASH).await?;
    /// # let code = "";
    ///
    /// // ... enter phone number, request login code ...
    ///
    /// let user = match client.sign_in(&token, &code).await {
    ///     Err(SignInError::PasswordRequired(password_token) ) => {
    ///         let mut password = get_user_password(password_token.hint().unwrap());
    ///
    ///         client
    ///             .check_password(password_token, password)
    ///             .await.unwrap()
    ///     }
    ///     Ok(user) => user,
    ///     Ok(_) => panic!("Sign in required"),
    ///     Err(err) => {
    ///         panic!("Failed to sign in as a user :(\n{err}");
    ///     }
    /// };
    /// # Ok(())
    /// # }
    /// ```
    /// CPU validation and proof generation use at most four blocking jobs.
    /// Cancelling started work lets it finish while retaining its admission slot.
    pub async fn check_password(
        &self,
        password_token: PasswordToken,
        password: impl AsRef<[u8]>,
    ) -> Result<User, SignInError> {
        let password: Arc<[u8]> = password.as_ref().into();
        let (mut password_info, mut check_password) =
            password_proof(password_token.password, password.clone())
                .await
                .map_err(SignInError::Other)?;
        if check_password.is_none() {
            let fresh = self
                .get_password_information()
                .await
                .map_err(SignInError::Other)?
                .password;
            (password_info, check_password) = password_proof(fresh, password)
                .await
                .map_err(SignInError::Other)?;
        }
        let check_password =
            check_password.ok_or_else(|| SignInError::Other(invalid_password_state()))?;

        match self.invoke(&check_password).await {
            Ok(tl::enums::auth::Authorization::Authorization(x)) => {
                self.complete_login(x).await.map_err(SignInError::Other)
            }
            Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => {
                Err(SignInError::SignUpRequired)
            }
            Err(err) if err.is("PASSWORD_HASH_INVALID") => {
                Err(SignInError::InvalidPassword(PasswordToken {
                    password: password_info,
                }))
            }
            Err(error) => Err(SignInError::Other(error)),
        }
    }

    /// Signs out of the account authorized by this client's session.
    ///
    /// If the client was not logged in, this method returns false.
    ///
    /// The client is not disconnected after signing out.
    ///
    /// Note that after using this method you will have to sign in again. If all you want to do
    /// is disconnect, simply [`drop`] the [`Client`] instance.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// if client.sign_out().await.is_ok() {
    ///     println!("Signed out successfully!");
    /// } else {
    ///     println!("No user was signed in, so nothing has changed...");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn sign_out(&self) -> Result<tl::enums::auth::LoggedOut, InvocationError> {
        self.invoke(&tl::functions::auth::LogOut {}).await
    }

    /// Signals all clients sharing the same sender pool to disconnect.
    pub fn disconnect(&self) {
        self.0.handle.quit();
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use grammers_mtsender::SenderPool;
    use grammers_session::storages::MemorySession;
    use std::sync::Arc;

    fn password_info() -> tl::types::account::Password {
        tl::types::account::Password {
            has_recovery: false,
            has_secure_values: false,
            has_password: true,
            current_algo: None,
            srp_b: None,
            srp_id: None,
            hint: Some("private-hint-fixture".into()),
            email_unconfirmed_pattern: None,
            new_algo: tl::enums::PasswordKdfAlgo::Unknown,
            new_secure_algo: tl::enums::SecurePasswordKdfAlgo::Unknown,
            secure_random: b"private-random-fixture".to_vec(),
            pending_reset_date: None,
            login_email_pattern: None,
        }
    }

    #[tokio::test]
    async fn missing_password_algorithm_returns_error_without_panicking() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 12345);
        let client = Client::new(pool.handle);
        let result = client
            .check_password(PasswordToken::new(password_info()), b"unused")
            .await;
        assert!(
            matches!(result, Err(SignInError::Other(InvocationError::Io(e))) if e.kind() == std::io::ErrorKind::InvalidData)
        );
    }

    #[tokio::test]
    async fn unknown_password_algorithm_returns_unsupported_without_panicking() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 12345);
        let client = Client::new(pool.handle);
        let mut info = password_info();
        info.current_algo = Some(tl::enums::PasswordKdfAlgo::Unknown);
        let result = client
            .check_password(PasswordToken::new(info), b"unused")
            .await;
        assert!(
            matches!(result, Err(SignInError::Other(InvocationError::Io(e))) if e.kind() == std::io::ErrorKind::Unsupported)
        );
    }

    #[test]
    fn password_token_debug_does_not_expose_password_state() {
        let debug = format!("{:?}", PasswordToken::new(password_info()));
        assert!(!debug.contains("private-hint-fixture"));
        assert!(!debug.contains("secure_random"));
    }
    fn supported_password_info() -> tl::types::account::Password {
        let mut info = password_info();
        info.current_algo = Some(
            tl::types::PasswordKdfAlgoSha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow {
                salt1: vec![1],
                salt2: vec![2],
                p: vec![255; 256],
                g: 3,
            }
            .into(),
        );
        info.srp_b = Some(vec![1; 256]);
        info.srp_id = Some(0);
        info
    }

    #[test]
    fn malformed_password_fields_are_rejected_before_crypto() {
        assert!(password_parameters(&supported_password_info()).is_ok());
        for case in 0..10 {
            let mut info = supported_password_info();
            match case {
                0 => info.srp_b = None,
                1 => info.srp_id = None,
                2 => info.srp_b = Some(vec![0]),
                3 => info.srp_b = Some(vec![1; 257]),
                4 => info.srp_b = Some(vec![0; 256]),
                5 => info.secure_random.clear(),
                6 => info.secure_random = vec![1; 257],
                7 => info.has_password = false,
                8 => info.secure_random = vec![0; 256],
                _ => info.current_algo = Some(
                    tl::types::PasswordKdfAlgoSha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow {
                        salt1: vec![],
                        salt2: vec![],
                        p: vec![255; 256],
                        g: 8,
                    }
                    .into(),
                ),
            }
            assert!(
                matches!(password_parameters(&info), Err(InvocationError::Io(e)) if e.kind() == io::ErrorKind::InvalidData),
                "case {case}"
            );
        }
    }

    #[test]
    fn password_b_encoding_length_matches_srp_reference_boundaries() {
        for len in [0, 1, 247, 248, 256, 257] {
            let mut info = supported_password_info();
            info.srp_b = Some(vec![1; len]);
            assert_eq!(
                password_parameters(&info).is_ok(),
                (248..=256).contains(&len),
                "length {len}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn srp_job_preserves_current_thread_progress() {
        let _guard = SRP_TEST_LOCK.lock().await;
        let (tx, rx) = std::sync::mpsc::channel();
        let job =
            run_srp_job(move || Ok(rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok()));
        let peer = async {
            tokio::task::yield_now().await;
            let _ = tx.send(());
        };
        let (result, ()) = tokio::join!(job, peer);
        assert!(result.unwrap(), "SRP job blocked the sole async worker");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn started_srp_jobs_keep_capacity_after_cancellation_and_proof_waits() {
        use std::time::Duration;
        let _guard = SRP_TEST_LOCK.lock().await;
        let mut callers = Vec::new();
        let mut release = Vec::new();
        for _ in 0..4 {
            let (started, entered) = tokio::sync::oneshot::channel();
            let (finish, blocking) = std::sync::mpsc::channel();
            callers.push(tokio::spawn(run_srp_job(move || {
                let _ = started.send(());
                blocking.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })));
            release.push(finish);
            tokio::time::timeout(Duration::from_secs(2), entered)
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(SRP_JOBS.available_permits(), 0);
        for caller in callers {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        }
        assert_eq!(SRP_JOBS.available_permits(), 0);
        let mut proof = tokio::spawn(password_proof(
            Box::new(supported_password_info()),
            Arc::from(&b"fixture"[..]),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut proof)
                .await
                .is_err()
        );
        proof.abort();
        assert!(matches!(proof.await, Err(error) if error.is_cancelled()));
        for finish in release {
            finish.send(()).unwrap();
        }
        let _all_finished = tokio::time::timeout(Duration::from_secs(2), SRP_JOBS.acquire_many(4))
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn missing_zero_and_overflow_migration_dc_are_rejected() {
        for value in [None, Some(0), Some(i32::MAX as u32 + 1), Some(u32::MAX)] {
            assert!(matches!(
                migration_dc(value),
                Err(InvocationError::InvalidDc)
            ));
        }
        assert_eq!(migration_dc(Some(2)).unwrap(), 2);
    }

    #[test]
    fn payment_required_is_actionable_and_redacted() {
        let result = login_code_response(
            tl::types::auth::SentCodePaymentRequired {
                store_product: "private-product".into(),
                phone_code_hash: "private-hash".into(),
                support_email_address: "private-email".into(),
                support_email_subject: "private-subject".into(),
                premium_days: 7,
                currency: "USD".into(),
                amount: 100,
            }
            .into(),
        );
        let Err(InvocationError::Io(error)) = result else {
            panic!("expected payment-required error")
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("official Telegram client"));
        assert!(!format!("{error:?}").contains("private-"));
    }

    #[test]
    fn unexpected_sent_code_success_does_not_create_login_token() {
        let result = login_code_response(
            tl::types::auth::SentCodeSuccess {
                authorization: tl::types::auth::AuthorizationSignUpRequired {
                    terms_of_service: None,
                }
                .into(),
            }
            .into(),
        );
        assert!(
            matches!(result, Err(InvocationError::Io(error)) if error.kind() == io::ErrorKind::InvalidData)
        );
    }

    #[tokio::test]
    async fn out_of_range_srp_b_refreshes_parameters_only_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tl::{Identifiable, Serializable};
        let _guard = SRP_TEST_LOCK.lock().await;
        for p_byte in [255, 254] {
            let mut info = supported_password_info();
            info.current_algo = Some(
                tl::types::PasswordKdfAlgoSha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow {
                    salt1: vec![1],
                    salt2: vec![2],
                    p: vec![p_byte; 256],
                    g: 3,
                }
                .into(),
            );
            info.srp_b = Some(vec![255; 256]);
            assert!(password_parameters(&info).is_ok());
            let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
            let mut client = Client::new(pool.handle);
            drop(pool.runner);
            let refreshes = Arc::new(AtomicUsize::new(0));
            let recorded = Arc::clone(&refreshes);
            Arc::get_mut(&mut client.0).unwrap().rpc_responder = Some(Arc::new(move |_, body| {
                assert_eq!(
                    u32::from_le_bytes(body[..4].try_into().unwrap()),
                    tl::functions::account::GetPassword::CONSTRUCTOR_ID
                );
                assert_eq!(
                    recorded.fetch_add(1, Ordering::Relaxed),
                    0,
                    "only one refresh is permitted"
                );
                Ok(tl::enums::account::Password::Password(supported_password_info()).to_bytes())
            }));
            let result = client
                .check_password(PasswordToken::new(info), b"unused")
                .await;
            assert!(
                matches!(result, Err(SignInError::Other(InvocationError::Io(error))) if error.kind() == io::ErrorKind::InvalidData)
            );
            assert_eq!(refreshes.load(Ordering::Relaxed), 1);
        }
    }
}
