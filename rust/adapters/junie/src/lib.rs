use ccusage_adapter_common::{filter_loaded_entries_by_date, read_files_parallel};
use ccusage_core::*;

mod loader;
mod parser;
mod paths;
mod report;

use crate::{
    PricingMap, Result, cli::AgentCommandArgs, print_json_or_jq, print_usage_table, wants_json,
};

pub use loader::load_entries;
pub(crate) use report::report_from_rows;
pub(crate) use report::summarize_daily_grouped_by_project;
pub use report::summarize_entries;

/// Runs a Junie usage report for the requested report kind.
pub fn run(args: AgentCommandArgs) -> Result<()> {
    let shared = args.shared;
    let pricing = PricingMap::load_with_overrides(
        shared.offline,
        crate::log_level() != Some(0),
        shared.pricing_overrides.iter(),
    );
    let mut entries = load_entries(&shared, &pricing)?;
    filter_loaded_entries_by_date(&mut entries, &shared);
    if let Some(project) = &args.project {
        entries.retain(|entry| entry.project.as_ref() == project);
    }
    let mut rows = if args.instances && args.kind == cli::AgentReportKind::Daily {
        summarize_daily_grouped_by_project(&entries)?
    } else {
        summarize_entries(&entries, args.kind)?
    };
    sort_summaries(&mut rows, &shared.order, ccusage_core::summary_period);
    if wants_json(&shared) {
        if args.instances && rows.iter().any(|row| row.project.is_some()) {
            let output = serde_json::json!({
                "projects": ccusage_report::group_project_output(&rows),
                "totals": totals_json(&rows),
            });
            return print_json_or_jq(output, shared.jq.as_deref(), shared.no_cost);
        }
        return print_json_or_jq(
            report_from_rows(&rows, args.kind),
            shared.jq.as_deref(),
            shared.no_cost,
        );
    }
    if rows.is_empty() {
        eprintln!("No Junie usage data found.");
        return Ok(());
    }
    print_usage_table(
        "Junie Token Usage Report",
        ccusage_core::first_column(args.kind),
        &rows,
        &shared,
        args.instances,
        args.project_aliases.as_deref(),
    )?;
    Ok(())
}
