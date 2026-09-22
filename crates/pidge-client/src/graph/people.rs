//! GET /me/people — Outlook's ranked "people I interact with" list, used to
//! resolve a display name or partial address to a full e-mail address.

use serde::Deserialize;

use crate::error::ClientError;

/// A person from `/me/people`, with the first scored e-mail address Graph
/// returned (entries without any scored address are skipped entirely).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub display_name: String,
    pub address: String,
}

#[derive(Deserialize)]
struct GraphPeopleList {
    value: Vec<GraphPerson>,
}

#[derive(Deserialize)]
struct GraphPerson {
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(rename = "scoredEmailAddresses", default)]
    scored_email_addresses: Vec<GraphScoredEmailAddress>,
}

#[derive(Deserialize)]
struct GraphScoredEmailAddress {
    address: String,
}

/// GET /me/people?$top={top}&$select=displayName,scoredEmailAddresses
pub async fn list_people(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    top: usize,
) -> Result<Vec<Person>, ClientError> {
    let url = format!("{base_url}/me/people");
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token).query(&[
        ("$top", top.to_string()),
        ("$select", "displayName,scoredEmailAddresses".to_string()),
    ]))
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let list: GraphPeopleList = resp.json().await?;
    Ok(list
        .value
        .into_iter()
        .filter_map(|p| {
            let address = p.scored_email_addresses.into_iter().next()?.address;
            Some(Person {
                display_name: p.display_name.unwrap_or_default(),
                address,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_people_maps_one_person() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/people"))
            .and(query_param("$top", "10"))
            .and(query_param("$select", "displayName,scoredEmailAddresses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "displayName": "Anna",
                        "scoredEmailAddresses": [{"address": "anna@example.com"}]
                    }
                ]
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let people = list_people(&http, &server.uri(), "AT", 10).await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].display_name, "Anna");
        assert_eq!(people[0].address, "anna@example.com");
    }

    #[tokio::test]
    async fn list_people_skips_entries_without_a_scored_address() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/people"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "displayName": "No Address", "scoredEmailAddresses": [] },
                    {
                        "displayName": "Anna",
                        "scoredEmailAddresses": [
                            {"address": "anna@example.com"},
                            {"address": "anna.alt@example.com"}
                        ]
                    }
                ]
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let people = list_people(&http, &server.uri(), "AT", 10).await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].address, "anna@example.com");
    }
}
