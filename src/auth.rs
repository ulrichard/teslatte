use crate::error::TeslatteError::{CouldNotFindCallbackCode, CouldNotFindState};
use crate::{OwnerApi, TeslatteError};
use derive_more::{Display, FromStr};
use rand::Rng;
#[cfg(feature = "async-interface")]
use reqwest::Client;
#[cfg(feature = "blocking-interface")]
use ureq::Agent;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{stdin, stdout, Write};
use url::Url;

const AUTHORIZE_URL: &str = "https://auth.tesla.com/oauth2/v3/authorize";
const TOKEN_URL: &str = "https://auth.tesla.com/oauth2/v3/token";

#[derive(Debug, Clone, Serialize, Deserialize, FromStr, Display)]
pub struct AccessToken(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, FromStr, Display)]
pub struct RefreshToken(pub String);

struct Callback {
    code: String,
    state: String,
}

impl OwnerApi {
    /// Show a URL for the user to click on to log into tesla.com, the ask them to paste the
    /// URL they end up on, which is a 404 page. The URL contains OAuth information needed to
    /// complete authentication for an access key.
    pub async fn from_interactive_url() -> Result<OwnerApi, TeslatteError> {
        let login_form = Self::get_login_url_for_user().await;
        println!("{}", "-".repeat(80));
        println!("{}", login_form.url);
        println!("{}", "-".repeat(80));
        println!(
            r#"Visit the URL above, and log in to your Tesla account if not already logged in.
After you log in (or already logged in), it will redirect you to a 404 error
page, where the URL will start with https://auth.tesla.com/void/callback?code=...
"#
        );
        let callback_url = ask_input("Enter the whole URL of the 404 page: ");
        println!(); // Newline to make the next output more separated and clear.

        OwnerApi::from_callback_url(&login_form, &callback_url).await
    }

    /// Generate a [LoginForm] containing a URL the user should visit.
    ///
    /// See [OwnerApi::from_callback_url()] for the next step.
    pub async fn get_login_url_for_user() -> LoginForm {
        let code = Code::new();
        let state = random_string(8);
        let url = Self::login_url(&code, &state);
        LoginForm { url, code, state }
    }

    /// Parse a callback URL that the user was redirected to after logging in via
    /// [OwnerApi::from_interactive_url()].
    pub async fn from_callback_url(
        login_form: &LoginForm,
        callback_url: &str,
    ) -> Result<OwnerApi, TeslatteError> {
        let callback = Self::extract_callback_from_url(callback_url)?;
        if callback.state != login_form.state {
            return Err(TeslatteError::StateMismatch {
                request: login_form.state.clone(),
                callback: callback.state,
            });
        }

        let bearer = Self::exchange_auth_for_bearer(&login_form.code, &callback.code).await?;
        let access_token = AccessToken(bearer.access_token);
        let refresh_token = RefreshToken(bearer.refresh_token);
        Ok(OwnerApi::new(access_token, Some(refresh_token)))
    }

    pub async fn from_refresh_token(
        refresh_token: &RefreshToken,
    ) -> Result<OwnerApi, TeslatteError> {
        let response = Self::refresh_token(refresh_token).await?;
        Ok(OwnerApi::new(
            response.access_token,
            Some(response.refresh_token),
        ))
    }

    async fn exchange_auth_for_bearer(
        code: &Code,
        callback_code: &str,
    ) -> Result<BearerTokenResponse, TeslatteError> {
        let url = TOKEN_URL;
        let payload = BearerTokenRequest {
            grant_type: "authorization_code".into(),
            client_id: "ownerapi".into(),
            code: callback_code.into(),
            code_verifier: code.verifier.clone(),
            redirect_uri: "https://auth.tesla.com/void/callback".into(),
        };
        Self::auth_post(url, &payload).await
    }

    /// Refresh the internally stored access token using the known refresh token.
    pub async fn refresh(&mut self) -> Result<(), TeslatteError> {
        match &self.refresh_token {
            None => Err(TeslatteError::NoRefreshToken),
            Some(refresh_token) => {
                let response = Self::refresh_token(refresh_token).await?;
                self.access_token = response.access_token;
                self.refresh_token = Some(response.refresh_token);
                Ok(())
            }
        }
    }

    pub async fn refresh_token(
        refresh_token: &RefreshToken,
    ) -> Result<RefreshTokenResponse, TeslatteError> {
        let url = "https://auth.tesla.com/oauth2/v3/token";
        let payload = RefreshTokenRequest {
            grant_type: "refresh_token".into(),
            client_id: "ownerapi".into(),
            refresh_token: refresh_token.0.clone(),
            scope: "openid email offline_access".into(),
        };
        Self::auth_post(url, &payload).await
    }

#[cfg(feature = "async-interface")]
    async fn auth_post<'a, S, D>(url: &str, payload: &S) -> Result<D, TeslatteError>
    where
        S: Serialize,
        D: DeserializeOwned,
    {
        let response = Client::new()
            .post(url)
            .header("Accept", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|source| TeslatteError::FetchError {
                source,
                request: url.to_string(),
            })?;

        let body = response
            .text()
            .await
            .map_err(|source| TeslatteError::FetchError {
                source,
                request: url.to_string(),
            })?;

        let json =
            serde_json::from_str::<D>(&body).map_err(|source| TeslatteError::DecodeJsonError {
                source,
                body: body.to_string(),
                request: url.to_string(),
            })?;

        Ok(json)
    }

#[cfg(feature = "blocking-interface")]
    async fn auth_post<'a, S, D>(url: &str, payload: &S) -> Result<D, TeslatteError>
    where
        S: Serialize,
        D: DeserializeOwned,
    {
        let response = Agent::new()
                .post(url)
                .set("Content-Type", "application/json")
                .set("Accept", "application/json")
              .send_json(payload)
            .map_err(|source| TeslatteError::FetchError {
                source,
                request: url.to_string(),
            })?;

        let body = response
            .into_string()
            .map_err(|source| TeslatteError::FetchError {
                source: source.into(),
                request: url.to_string(),
            })?;

        let json =
            serde_json::from_str::<D>(&body).map_err(|source| TeslatteError::DecodeJsonError {
                source,
                body: body.to_string(),
                request: url.to_string(),
            })?;

        Ok(json)
    }

    pub fn login_url(code: &Code, state: &str) -> String {
        let mut url = Url::parse(AUTHORIZE_URL).unwrap();
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", "ownerapi");
        query.append_pair("code_challenge", &code.challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("redirect_uri", "https://auth.tesla.com/void/callback");
        query.append_pair("response_type", "code");
        query.append_pair("scope", "openid email offline_access");
        query.append_pair("state", state);
        drop(query);
        url.to_string()
    }

    fn extract_callback_from_url(callback_url: &str) -> Result<Callback, TeslatteError> {
        let url =
            Url::parse(callback_url).map_err(TeslatteError::UserDidNotSupplyValidCallbackUrl)?;
        let pairs = url.query_pairs().collect::<Vec<_>>();

        let code = pairs
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .ok_or(CouldNotFindCallbackCode)?;

        let state = pairs
            .iter()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())
            .ok_or(CouldNotFindState)?;

        Ok(Callback { code, state })
    }
}

#[derive(Debug, Serialize)]
struct RefreshTokenRequest {
    grant_type: String,
    client_id: String,
    refresh_token: String,
    scope: String,
}

#[derive(Debug, Deserialize)]
pub struct RefreshTokenResponse {
    pub access_token: AccessToken,
    pub refresh_token: RefreshToken,
    pub id_token: String,
    pub expires_in: u32,
    pub token_type: String,
}

#[derive(Debug, Default)]
pub struct LoginForm {
    #[allow(dead_code)]
    pub url: String,
    pub code: Code,
    pub state: String,
}

#[derive(Debug, Serialize)]
struct BearerTokenRequest {
    grant_type: String,
    client_id: String,
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

#[derive(Debug, Deserialize)]
struct BearerTokenResponse {
    access_token: String,
    refresh_token: String,

    #[allow(dead_code)]
    expires_in: u32,

    #[allow(dead_code)]
    state: String,

    #[allow(dead_code)]
    token_type: String,

    #[allow(dead_code)]
    id_token: String,
}

#[derive(Debug, Default)]
pub struct Code {
    verifier: String,
    challenge: String,
}

impl Code {
    fn new() -> Self {
        let verifier = pkce::code_verifier(86);
        let challenge = pkce::code_challenge(&verifier);

        // Unwrap should be OK here, since code_verifier() generates bytes from ASCII.
        let verifier = String::from_utf8(verifier).unwrap();

        Self {
            verifier,
            challenge,
        }
    }
}

fn random_string(len: usize) -> String {
    let mut rng = rand::thread_rng();
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        s.push(rng.gen_range(b'a'..=b'z') as char);
    }
    s
}

pub fn ask_input(prompt: &str) -> String {
    print!("{}", prompt);
    let mut s = String::new();
    stdout()
        .flush()
        .expect("Failed to flush while expecting user input.");
    stdin()
        .read_line(&mut s)
        .expect("Failed to read line of user input.");
    s.trim().to_string()
}
