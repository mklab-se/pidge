//! GET /me/calendars — list calendars on the signed-in account.

use pidge_core::Calendar;
use serde::Deserialize;

use crate::error::ClientError;

#[derive(Deserialize)]
struct GraphCalendarList {
    value: Vec<GraphCalendar>,
}

#[derive(Deserialize)]
struct GraphCalendar {
    id: String,
    name: Option<String>,
    #[serde(rename = "isDefaultCalendar", default)]
    is_default_calendar: Option<bool>,
    #[serde(default)]
    color: Option<String>,
    #[serde(rename = "canEdit", default)]
    can_edit: Option<bool>,
}

pub async fn list_calendars(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    account: &str,
) -> Result<Vec<Calendar>, ClientError> {
    let url = format!("{base_url}/me/calendars");
    let resp = super::send_with_retry(http.get(&url).bearer_auth(access_token)).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ClientError::Graph {
            status: status.as_u16(),
            message: text,
        });
    }
    let list: GraphCalendarList = resp.json().await?;
    Ok(list
        .value
        .into_iter()
        .map(|g| Calendar {
            account: account.to_string(),
            id: g.id,
            name: g.name.unwrap_or_default(),
            is_default: g.is_default_calendar.unwrap_or(false),
            color: g.color,
            can_edit: g.can_edit.unwrap_or(true),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_calendars_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/calendars"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    {
                        "id": "cal-1",
                        "name": "Calendar",
                        "isDefaultCalendar": true,
                        "color": "auto",
                        "canEdit": true
                    },
                    {
                        "id": "cal-2",
                        "name": "Birthdays",
                        "isDefaultCalendar": false,
                        "canEdit": false
                    }
                ]
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let cals = list_calendars(&http, &server.uri(), "AT", "u@e.com")
            .await
            .unwrap();
        assert_eq!(cals.len(), 2);
        assert_eq!(cals[0].name, "Calendar");
        assert!(cals[0].is_default);
        assert!(cals[0].can_edit);
        assert_eq!(cals[1].name, "Birthdays");
        assert!(!cals[1].can_edit);
    }
}
