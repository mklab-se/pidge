//! `mail_categories`: the user's review categories, talked through with
//! the harness: list, confirm the defaults, add, rename, describe, remove,
//! reorder, and teach by rule (a sender or domain) or by example (a real
//! message, copied by id from the user's own mailbox).

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, schemars, tool, tool_router};
use serde::{Deserialize, Serialize};

use super::PidgeMcp;
use super::mail_read::check_id;
use crate::context::{ToolContext, store_error, tool_error};
use crate::render::{one_line, untrusted, who};
use crate::review::categories::{
    self as cats, Category, Example, MAX_CATEGORIES, MAX_EXAMPLES, MAX_RULES, OTHER,
};
use crate::users::{UserRecord, user_hash};

/// The user record must stay well under Key Vault's 25 KB secret limit.
const MAX_RECORD_BYTES: usize = 24_000;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CategoriesAction {
    #[default]
    List,
    /// The user is happy with the categories in use.
    Confirm,
    /// Back to the default categories (drops the user's own).
    UseDefaults,
    Add,
    /// Rename (`new_name`), describe (`description`) or toggle
    /// `bulk_suggestion` on `name`.
    Update,
    Remove,
    AddRule,
    RemoveRule,
    /// Add the message `id` as an example of `name`.
    AddExample,
    /// Remove example number `example` (as listed) from `name`.
    RemoveExample,
    /// Put the categories in `order` (names); unnamed ones keep their order after.
    Reorder,
}

#[derive(Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CategoriesArgs {
    #[serde(default)]
    pub action: Option<CategoriesAction>,
    /// The category acted on.
    #[serde(default)]
    pub name: Option<String>,
    /// For `update`: the new name.
    #[serde(default)]
    pub new_name: Option<String>,
    /// For `add`/`update`: what belongs in the category, in the user's words.
    #[serde(default)]
    pub description: Option<String>,
    /// For `add_rule`/`remove_rule`: `sender:<address>` or `domain:<domain>`.
    #[serde(default)]
    pub rule: Option<String>,
    /// For `add_example`: a message id from the review or mail_overview.
    #[serde(default)]
    pub id: Option<String>,
    /// For `remove_example`: the example's number as listed.
    #[serde(default)]
    pub example: Option<usize>,
    /// For `add`/`update`: offer "archive all of these" at review start.
    #[serde(default)]
    pub bulk_suggestion: Option<bool>,
    /// For `reorder`: category names in the new order.
    #[serde(default)]
    pub order: Option<Vec<String>>,
}

#[tool_router(router = categories_router, vis = "pub(crate)")]
impl PidgeMcp {
    #[tool(
        description = "E-mail: the categories mail is sorted into during a review (mail_review_start), in review order. Talk them through with the user: `confirm` keeps the ones in use, `add`/`update`/`remove`/`reorder` edit them, `add_rule` pins a sender or domain to a category (e.g. \"everything from the climbing club is Leisure\" → rule domain:climbingclub.se), `add_example` teaches by a real message id (\"mail like this one\"), `use_defaults` goes back to the standard set. `Other` is built in for whatever fits nowhere. Returns the updated list."
    )]
    async fn mail_categories(
        &self,
        Parameters(args): Parameters<CategoriesArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tc = ToolContext::from_request(&self.state, &ctx).await?;
        let action = args.action.unwrap_or_default();
        if action == CategoriesAction::List {
            return Ok(ok(render(&tc.record)));
        }

        // An example is read before the record is changed, so a missing
        // message changes nothing.
        let example = match action {
            CategoriesAction::AddExample => {
                let id = args
                    .id
                    .as_deref()
                    .ok_or_else(|| tool_error("add_example needs `id`"))?;
                check_id(id)?;
                let m = self.find_message(&tc.accounts(None)?, id).await?;
                Some(Example::new(&who(&m.from), &m.subject))
            }
            _ => None,
        };

        let signin = tc.record.signin.clone();
        let mut record = self
            .state
            .users
            .load(&signin)
            .await
            .map_err(|e| store_error("loading user record", &signin, e))?
            .unwrap_or_else(|| tc.record.clone());
        let mut list = record.review.categories();
        let receipt = edit(&mut list, action, &args, example)?;
        record.review.categories = match action {
            CategoriesAction::UseDefaults => None,
            _ => Some(list),
        };
        record.review.confirmed = true;
        let size = serde_json::to_string(&record).map(|s| s.len()).unwrap_or(0);
        if size > MAX_RECORD_BYTES {
            return Err(tool_error(
                "That would make the categories too large to store; remove some examples or rules first",
            ));
        }
        self.state
            .users
            .save(&record)
            .await
            .map_err(|e| store_error("saving categories", &signin, e))?;
        self.state.cache.invalidate_user(&signin);
        tracing::info!(user = %user_hash(&signin), action = ?action, "categories updated");
        Ok(ok(format!("{receipt}\n\n{}", render(&record))))
    }
}

/// Applies one edit to `list`; returns a one-line receipt.
fn edit(
    list: &mut Vec<Category>,
    action: CategoriesAction,
    args: &CategoriesArgs,
    example: Option<Example>,
) -> Result<String, McpError> {
    let name = || {
        args.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| tool_error("this action needs `name`"))
    };
    let find = |list: &[Category], n: &str| {
        cats::position(list, n).ok_or_else(|| {
            tool_error(format!(
                "No category named {}; the list shows the current ones",
                one_line(n)
            ))
        })
    };
    let description = args
        .description
        .as_deref()
        .map(cats::clean_description)
        .transpose()
        .map_err(tool_error)?;
    Ok(match action {
        CategoriesAction::List => String::new(),
        CategoriesAction::Confirm => "Confirmed.".into(),
        CategoriesAction::UseDefaults => "Back to the default categories.".into(),
        CategoriesAction::Add => {
            let n = cats::clean_name(name()?).map_err(tool_error)?;
            if cats::position(list, &n).is_some() {
                return Err(tool_error(format!(
                    "{n} already exists; use update to change it"
                )));
            }
            if list.len() >= MAX_CATEGORIES {
                return Err(tool_error(format!(
                    "At most {MAX_CATEGORIES} categories; remove one first"
                )));
            }
            let mut c: Category = serde_json::from_value(serde_json::json!({ "name": n }))
                .expect("a name-only category deserializes");
            c.description = description.unwrap_or_default();
            c.bulk_suggestion = args.bulk_suggestion.unwrap_or(false);
            list.push(c);
            format!("Added {n}.")
        }
        CategoriesAction::Update => {
            let i = find(list, name()?)?;
            if let Some(new) = args.new_name.as_deref() {
                let new = cats::clean_name(new).map_err(tool_error)?;
                if cats::position(list, &new).is_some_and(|j| j != i) {
                    return Err(tool_error(format!("{new} already exists")));
                }
                list[i].name = new;
            }
            if let Some(d) = description {
                list[i].description = d;
            }
            if let Some(b) = args.bulk_suggestion {
                list[i].bulk_suggestion = b;
            }
            format!("Updated {}.", list[i].name)
        }
        CategoriesAction::Remove => {
            let i = find(list, name()?)?;
            let removed = list.remove(i);
            format!(
                "Removed {}; its mail now goes to {OTHER} or another category.",
                removed.name
            )
        }
        CategoriesAction::AddRule | CategoriesAction::RemoveRule => {
            let i = find(list, name()?)?;
            let rule =
                cats::parse_rule(args.rule.as_deref().unwrap_or_default()).map_err(tool_error)?;
            if action == CategoriesAction::RemoveRule {
                list[i].rules.retain(|r| *r != rule);
                format!("Removed the rule {rule} from {}.", list[i].name)
            } else {
                // A sender or domain belongs to one category only.
                for c in list.iter_mut() {
                    c.rules.retain(|r| *r != rule);
                }
                if list[i].rules.len() >= MAX_RULES {
                    return Err(tool_error(format!(
                        "At most {MAX_RULES} rules per category; remove one first"
                    )));
                }
                list[i].rules.push(rule.clone());
                format!("Mail matching {rule} now goes to {}.", list[i].name)
            }
        }
        CategoriesAction::AddExample => {
            let i = find(list, name()?)?;
            let example = example.expect("read before editing");
            let c = &mut list[i];
            if c.examples.len() >= MAX_EXAMPLES {
                c.examples.remove(0);
            }
            c.examples.push(example);
            format!("Added the example to {}.", c.name)
        }
        CategoriesAction::RemoveExample => {
            let i = find(list, name()?)?;
            let k = args
                .example
                .ok_or_else(|| tool_error("remove_example needs `example`"))?;
            if k == 0 || k > list[i].examples.len() {
                return Err(tool_error(format!("{} has no example {k}", list[i].name)));
            }
            list[i].examples.remove(k - 1);
            format!("Removed example {k} from {}.", list[i].name)
        }
        CategoriesAction::Reorder => {
            let order = args
                .order
                .as_ref()
                .ok_or_else(|| tool_error("reorder needs `order`"))?;
            let mut sorted = Vec::new();
            for n in order {
                let i = find(list, n)?;
                sorted.push(list.remove(i));
            }
            sorted.append(list);
            *list = sorted;
            "Reordered.".into()
        }
    })
}

fn render(record: &UserRecord) -> String {
    let list = record.review.categories();
    let state = match (&record.review.categories, record.review.confirmed) {
        (None, false) => " (the defaults; the user has not confirmed them yet)",
        (None, true) => " (the defaults)",
        _ => "",
    };
    let mut out = format!("Categories{state}:");
    for (n, c) in list.iter().enumerate() {
        out.push_str(&format!("\n{}. {}", n + 1, c.name));
        if !c.description.is_empty() {
            out.push_str(&format!(": {}", c.description));
        }
        if c.bulk_suggestion {
            out.push_str(" [offered for bulk clean-up]");
        }
        if !c.rules.is_empty() {
            out.push_str(&format!("\n   rules: {}", c.rules.join(", ")));
        }
        if !c.examples.is_empty() {
            out.push_str(&format!("\n   examples: {}", c.examples.len()));
        }
    }
    out.push_str(&format!(
        "\n{OTHER}: whatever fits no other category (built in)"
    ));
    // Examples are copied from received mail: third-party text.
    let examples: Vec<String> = list
        .iter()
        .flat_map(|c| {
            c.examples.iter().enumerate().map(move |(k, e)| {
                format!(
                    "{} example {}: {} / {}",
                    c.name,
                    k + 1,
                    one_line(&e.from),
                    one_line(&e.subject)
                )
            })
        })
        .collect();
    if !examples.is_empty() {
        out.push_str("\n\nExamples:\n");
        out.push_str(&untrusted(&examples.join("\n")));
    }
    out
}

fn ok(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::{ToolHarness, text};

    async fn call(h: &ToolHarness, args: CategoriesArgs) -> Result<String, McpError> {
        h.mcp
            .mail_categories(Parameters(args), h.ctx())
            .await
            .map(|r| text(&r))
    }

    fn args(action: CategoriesAction) -> CategoriesArgs {
        CategoriesArgs {
            action: Some(action),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_defaults_are_listed_until_confirmed() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        let out = call(&h, CategoriesArgs::default()).await.unwrap();
        assert!(out.contains("has not confirmed them yet"), "{out}");
        assert!(out.contains("1. Important"), "{out}");
        assert!(out.contains("Other: whatever fits"), "{out}");
        call(&h, args(CategoriesAction::Confirm)).await.unwrap();
        let rec = h.record().await;
        assert!(rec.review.confirmed);
        assert!(rec.review.categories.is_some());
    }

    #[tokio::test]
    async fn add_rule_and_rename_flow() {
        let h = ToolHarness::new(&["jane@example.com"]).await;
        let mut a = args(CategoriesAction::Add);
        a.name = Some("Leisure".into());
        a.description = Some("Climbing, photography and travel".into());
        call(&h, a).await.unwrap();

        let mut r = args(CategoriesAction::AddRule);
        r.name = Some("leisure".into());
        r.rule = Some("@climbingclub.se".into());
        let out = call(&h, r).await.unwrap();
        assert!(
            out.contains("domain:climbingclub.se now goes to Leisure"),
            "{out}"
        );

        // The same rule on another category moves it there.
        let mut r = args(CategoriesAction::AddRule);
        r.name = Some("Personal".into());
        r.rule = Some("domain:climbingclub.se".into());
        call(&h, r).await.unwrap();
        let cats = h.record().await.review.categories.unwrap();
        let leisure = cats.iter().find(|c| c.name == "Leisure").unwrap();
        assert!(leisure.rules.is_empty());

        let mut u = args(CategoriesAction::Update);
        u.name = Some("Leisure".into());
        u.new_name = Some("Other".into());
        assert!(call(&h, u).await.is_err(), "Other is reserved");
    }

    #[tokio::test]
    async fn an_example_is_copied_from_the_users_own_message() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        let h = ToolHarness::new(&["jane@example.com"]).await;
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages/AAMk1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "AAMk1",
                "subject": "Training on Tuesday",
                "from": { "emailAddress": { "name": "Climbing Club", "address": "info@climbingclub.se" } },
                "toRecipients": [], "ccRecipients": [], "bccRecipients": [],
                "receivedDateTime": "2026-10-11T06:00:00Z",
                "sentDateTime": "2026-10-11T06:00:00Z",
                "isRead": false, "hasAttachments": false,
                "body": { "contentType": "text", "content": "Hi" }
            })))
            .mount(&h.graph)
            .await;
        let mut e = args(CategoriesAction::AddExample);
        e.name = Some("Personal".into());
        e.id = Some("AAMk1".into());
        let out = call(&h, e).await.unwrap();
        assert!(
            out.contains(
                "Personal example 1: Climbing Club <info@climbingclub.se> / Training on Tuesday"
            ),
            "{out}"
        );

        let mut bad = args(CategoriesAction::AddExample);
        bad.name = Some("Personal".into());
        bad.id = Some("../x".into());
        assert!(call(&h, bad).await.is_err());
    }
}
