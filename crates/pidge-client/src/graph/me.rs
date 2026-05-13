//! GET /me — fetch the signed-in user's identity.

use serde::Deserialize;

use crate::error::ClientError;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub id: String,
    pub user_principal_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub mail: Option<String>,
}

pub async fn get_me(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
) -> Result<Me, ClientError> {
    let url = format!("{base_url}/me");
    let resp = http.get(&url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    Ok(resp.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_me_returns_identity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "user-id",
                "userPrincipalName": "kristofer@mklab.se",
                "displayName": "Kristofer Liljeblad",
                "mail": "kristofer@mklab.se"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let me = get_me(&http, &server.uri(), "AT").await.unwrap();
        assert_eq!(me.id, "user-id");
        assert_eq!(me.user_principal_name, "kristofer@mklab.se");
    }
}
