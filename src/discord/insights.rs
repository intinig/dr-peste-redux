//! `/insights [category]` — surfaces the learned ValueModel: which mods drive
//! price for a category, scoped to the active league. Read-only; open to
//! everyone (non-secret market data).

use super::{Context, Error};
use crate::trade::value::{canonical_category, CategoryModel, MIN_CATEGORY_SAMPLE};
use futures::Stream;
use poise::serenity_prelude as serenity;

/// One calibration line per category, e.g.
/// `Staff: n=2087 · model 75% · base 88% · skill +15% ✓ (beats baseline)`
/// or `Amulet: n=1206 · model 75% · base 76% · skill −1% ✗ (no skill over baseline)`.
/// Metrics show `n/a` when absent.
pub fn calibration_line(cat: &CategoryModel) -> String {
    let pct = |x: Option<f64>| match x {
        Some(v) => format!("{:.0}%", v * 100.0),
        None => "n/a".to_string(),
    };
    let skill = match cat.calibration.skill {
        Some(s) => format!("{:+.0}%", s * 100.0),
        None => "n/a".to_string(),
    };
    let (mark, verdict) = if cat.is_trusted() {
        ("✓", "beats baseline")
    } else {
        ("✗", "no skill over baseline")
    };
    format!(
        "{}: n={} · model {} · base {} · skill {} {} ({})",
        cat.category,
        cat.sample_size,
        pct(cat.calibration.model_err),
        pct(cat.calibration.baseline_err),
        skill,
        mark,
        verdict,
    )
}

/// The active league name from the store snapshot, if the bot has warmed up.
async fn current_league(ctx: &Context<'_>) -> Option<String> {
    ctx.data()
        .store
        .snapshot()
        .await
        .map(|s| s.league.name.clone())
}

/// Autocomplete: canonical category names with data in the active league,
/// substring-matched.
pub async fn autocomplete_insights_category<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Stream<Item = String> + 'a {
    let p = partial.to_lowercase();
    let names: Vec<String> = match current_league(&ctx).await {
        Some(league) => {
            let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
            model
                .categories_sorted(&league)
                .into_iter()
                .map(|c| c.category.clone())
                .filter(|name| name.to_lowercase().contains(&p))
                .take(25)
                .collect()
        }
        None => Vec::new(),
    };
    futures::stream::iter(names)
}

/// Formats the undersampled-gate section for a category's insights body.
/// Returns an empty string when there are no gate candidates.
pub fn gate_section(
    gates: &[crate::trade::value::gates::GateCandidate],
    catalog: &crate::trade::stats::StatCatalog,
) -> String {
    if gates.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n**Undersampled gates** (need more data):\n");
    for g in gates.iter().take(8) {
        // Show the human label; fall back to the raw stat_id (in backticks) only when
        // it can't be resolved. The operator selects the gate from `/harvest mod:`
        // autocomplete, so the raw id no longer needs to be shown for manual copy.
        match g.label.as_deref().or_else(|| catalog.label_for(&g.stat_id)) {
            Some(lbl) => out.push_str(&format!("• {} (n={})\n", lbl, g.count)),
            None => out.push_str(&format!("• `{}` (n={})\n", g.stat_id, g.count)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::stats::StatCatalog;
    use crate::trade::value::gates::GateCandidate;

    fn gate(stat_id: &str, label: Option<&str>, count: usize) -> GateCandidate {
        GateCandidate {
            stat_id: stat_id.into(),
            label: label.map(str::to_owned),
            count,
        }
    }

    #[test]
    fn gate_section_non_empty_when_gates_present() {
        let catalog = StatCatalog::default();
        let gates = vec![
            gate("explicit.stat_1234", Some("increased Fire Damage"), 3),
            gate("explicit.stat_5678", None, 7),
        ];
        let section = gate_section(&gates, &catalog);
        assert!(
            section.contains("Undersampled gates"),
            "section header missing: {section}"
        );
        assert!(
            section.contains("increased Fire Damage"),
            "label missing: {section}"
        );
        assert!(section.contains("n=3"), "count missing: {section}");
        // Falls back to the raw stat_id only when the label is unknown.
        assert!(
            section.contains("explicit.stat_5678"),
            "fallback id missing: {section}"
        );
        // A labeled gate shows ONLY its label — the raw stat_id is not printed
        // (the operator picks it from `/harvest mod:` autocomplete).
        assert!(
            !section.contains("explicit.stat_1234"),
            "labeled gate must not print its raw stat_id: {section}"
        );
    }

    #[test]
    fn gate_section_empty_when_no_gates() {
        let catalog = StatCatalog::default();
        assert_eq!(gate_section(&[], &catalog), "");
    }

    fn cat(
        name: &str,
        n: usize,
        model: Option<f64>,
        base: Option<f64>,
        skill: Option<f64>,
    ) -> crate::trade::value::CategoryModel {
        crate::trade::value::CategoryModel {
            category: name.into(),
            sample_size: n,
            calibration: crate::trade::value::backtest::Calibration {
                model_err: model,
                baseline_err: base,
                skill,
            },
            ..Default::default()
        }
    }

    #[test]
    fn calibration_line_shows_skill_and_beats_verdict() {
        let line = calibration_line(&cat("Staff", 2087, Some(0.75), Some(0.88), Some(0.15)));
        assert!(line.contains("n=2087"), "{line}");
        assert!(line.contains("model 75%"), "{line}");
        assert!(line.contains("base 88%"), "{line}");
        assert!(line.contains("skill"), "{line}");
        assert!(line.contains("15%"), "{line}");
        assert!(
            line.contains("✓"),
            "positive skill + samples → trusted mark: {line}"
        );
    }

    #[test]
    fn calibration_line_marks_no_skill() {
        let line = calibration_line(&cat("Amulet", 1206, Some(0.75), Some(0.76), Some(-0.01)));
        assert!(line.contains("✗"), "negative skill → not trusted: {line}");
    }

    #[test]
    fn calibration_line_handles_missing_metrics() {
        let line = calibration_line(&cat("Wand", 12, None, None, None));
        assert!(line.contains("n/a"), "{line}");
        assert!(line.contains("✗"), "{line}");
    }
}

/// Show learned value-drivers for a category (or list categories with no arg).
#[poise::command(slash_command)]
pub async fn insights(
    ctx: Context<'_>,
    #[description = "Item category (e.g. Staff). Omit to list categories."]
    #[autocomplete = "autocomplete_insights_category"]
    category: Option<String>,
) -> Result<(), Error> {
    let Some(league) = current_league(&ctx).await else {
        ctx.say("Still warming up — try again in a few seconds.")
            .await?;
        return Ok(());
    };

    // Build the embed under the value-model lock, then drop the guard before .await.
    let embed: serenity::CreateEmbed = {
        let model = ctx.data().value.read().unwrap_or_else(|e| e.into_inner());
        match category.as_deref() {
            None => {
                // Menu: only categories trusted enough to give reliable insights.
                let trusted: Vec<_> = model
                    .categories_sorted(&league)
                    .into_iter()
                    .filter(|c| c.sample_size >= MIN_CATEGORY_SAMPLE)
                    .collect();
                if trusted.is_empty() {
                    serenity::CreateEmbed::default().title("Market insights").description(format!(
                        "No category has enough data yet for **{league}** (need ≥{MIN_CATEGORY_SAMPLE} listings). Run `/harvest <category>` to warm one up."
                    ))
                } else {
                    let mut lines = String::new();
                    for c in trusted.iter().take(25) {
                        lines.push_str(&format!("• {}\n", calibration_line(c)));
                    }
                    lines.push_str("\nPass one, e.g. `/insights category:Staff`.");
                    serenity::CreateEmbed::default()
                        .title(format!("Market insights — {league}"))
                        .description(lines)
                }
            }
            Some(raw) => {
                let canon = canonical_category(raw);
                match model.category(&league, &canon) {
                    None => serenity::CreateEmbed::default()
                        .title(canon.clone())
                        .description(format!("No market data yet for **{canon}** in {league}.")),
                    Some(cat) if cat.sample_size < MIN_CATEGORY_SAMPLE => {
                        serenity::CreateEmbed::default().title(canon.clone()).description(format!(
                            "Only {} listings for **{canon}** in {league} so far (need ≥{MIN_CATEGORY_SAMPLE} for reliable insights). Harvest more.",
                            cat.sample_size
                        ))
                    }
                    Some(cat) => {
                        // Clone so we can drop the model read-guard before the catalog lookup.
                        let cat = cat.clone();
                        let (sample_size, base_median) = (cat.sample_size, cat.base_median);
                        drop(model);
                        let catalog = ctx.data().pricer.catalog();
                        // Resolve label: pre-stored label, else reverse-lookup via
                        // the catalog, else fall back to the raw stat id.
                        let label = |s_id: &str, s_label: &Option<String>| -> String {
                            s_label
                                .as_deref()
                                .or_else(|| catalog.label_for(s_id))
                                .unwrap_or(s_id)
                                .to_string()
                        };

                        let mut body = String::from("**Value drivers** (independent lift in parens):\n");
                        let mut any = false;
                        for s in cat.drivers().take(8) {
                            any = true;
                            let cond = match s.conditional_lift {
                                Some(c) => format!(" (independent {c:.1}×)"),
                                None => String::new(),
                            };
                            body.push_str(&format!(
                                "• **{}** — {:.1}× ({:.1} div){} · in {:.0}% of priciest · n={}\n",
                                label(&s.stat_id, &s.label),
                                s.lift,
                                s.median_with,
                                cond,
                                s.top_decile_freq * 100.0,
                                s.count
                            ));
                        }
                        if !any {
                            body.push_str("_(no mod clears the value-driver threshold yet)_\n");
                        }
                        if !cat.cooccurrences.is_empty() {
                            body.push_str("\n**Top combos on expensive items:**\n");
                            for p in cat.cooccurrences.iter().take(5) {
                                body.push_str(&format!(
                                    "• {} + {} (n={})\n",
                                    label(&p.a, &None),
                                    label(&p.b, &None),
                                    p.count
                                ));
                            }
                        }
                        body.push_str(&gate_section(&cat.undersampled_gates, catalog));
                        serenity::CreateEmbed::default()
                            .title(format!("{canon} — value drivers"))
                            .description(body)
                            .footer(serenity::CreateEmbedFooter::new(format!(
                                "{sample_size} listings · median {base_median:.1} div · {league}"
                            )))
                    }
                }
            }
        }
    };
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
