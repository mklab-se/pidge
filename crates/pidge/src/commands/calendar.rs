//! Top-level dispatch for `pidge calendar <subcommand>`.

use anyhow::Result;

use crate::cli::{CalendarCommands, CalendarsCommands};

pub async fn run(command: Option<CalendarCommands>, json: bool) -> Result<()> {
    match command {
        None => crate::commands::calendar_list::run_default(json).await,
        Some(CalendarCommands::List {
            account,
            from,
            to,
            today,
            tomorrow,
            week,
            month,
            calendar,
            limit,
            compact,
            table,
            cursor,
        }) => {
            crate::commands::calendar_list::run(
                account, from, to, today, tomorrow, week, month, calendar, limit, compact, table,
                json, cursor,
            )
            .await
        }
        Some(CalendarCommands::Show { fragment }) => {
            crate::commands::calendar_show::run(&fragment, json).await
        }
        Some(CalendarCommands::Search {
            query,
            account,
            calendar,
            limit,
            from,
            to,
        }) => {
            crate::commands::calendar_search::run(query, account, calendar, limit, from, to, json)
                .await
        }
        Some(CalendarCommands::Calendars { command }) => {
            let cmd = command.unwrap_or(CalendarsCommands::List);
            crate::commands::calendar_calendars::run(cmd, json).await
        }
        Some(CalendarCommands::New(args)) => crate::commands::calendar_new::run(args, json).await,
        Some(CalendarCommands::Edit { fragment, new }) => {
            crate::commands::calendar_edit::run(&fragment, new, json).await
        }
        Some(CalendarCommands::MoveTime {
            fragment,
            start,
            end,
            notify,
            no_notify,
            series,
        }) => {
            crate::commands::calendar_move_time::run(
                &fragment,
                &start,
                end.as_deref(),
                notify,
                no_notify,
                series,
                json,
            )
            .await
        }
        Some(CalendarCommands::Duplicate {
            fragment,
            start,
            title,
            calendar,
        }) => {
            crate::commands::calendar_duplicate::run(
                &fragment,
                start.as_deref(),
                title.as_deref(),
                calendar.as_deref(),
                json,
            )
            .await
        }
        Some(CalendarCommands::Delete {
            fragment,
            yes,
            series,
        }) => crate::commands::calendar_delete::run(&fragment, yes, series, json).await,
        Some(CalendarCommands::Cancel {
            fragment,
            comment,
            yes,
            series,
        }) => {
            crate::commands::calendar_cancel::run(
                &fragment,
                comment.as_deref().unwrap_or(""),
                yes,
                series,
                json,
            )
            .await
        }
        Some(CalendarCommands::Move { fragment, to }) => {
            crate::commands::calendar_move::run(&fragment, &to, json).await
        }
        Some(CalendarCommands::Rsvp {
            fragment,
            accept,
            tentative,
            decline,
            comment,
            no_notify,
        }) => {
            crate::commands::calendar_rsvp::run(
                &fragment,
                accept,
                tentative,
                decline,
                comment.as_deref().unwrap_or(""),
                !no_notify,
                json,
            )
            .await
        }
    }
}
