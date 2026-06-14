use colored::Colorize;
use std::collections::HashMap;
use std::time::Duration;

use crate::collector::TestItem;
use crate::runner::{CoverageInfo, TestResult, TestStatus};

pub fn print_header(total: usize, skipped: usize, workers: usize, with_coverage: bool) {
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
    println!(
        "  {} {}",
        "tiderace".bold().cyan(),
        "⚡ Rust-powered test engine".dimmed()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
    println!(
        "  {} {}   {} {}   {} {}   {} {}",
        "tests:".dimmed(),
        total.to_string().bold(),
        "skipped (unchanged):".dimmed(),
        skipped.to_string().yellow().bold(),
        "workers:".dimmed(),
        workers.to_string().bold(),
        "coverage:".dimmed(),
        if with_coverage {
            "on".green().bold()
        } else {
            "off".dimmed()
        }
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
    println!();
}

pub fn print_summary(
    results: &[TestResult],
    skipped_tests: &[TestItem],
    elapsed: Duration,
    coverage: Option<&HashMap<String, CoverageInfo>>,
) {
    let passed = results
        .iter()
        .filter(|r| r.status == TestStatus::Passed)
        .count();
    let failed = results
        .iter()
        .filter(|r| r.status == TestStatus::Failed)
        .count();
    let errors = results
        .iter()
        .filter(|r| r.status == TestStatus::Error)
        .count();
    let skipped_run = results
        .iter()
        .filter(|r| r.status == TestStatus::Skipped)
        .count();

    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
    println!("  {}", "Results".bold());
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );

    if passed > 0 {
        println!(
            "  {} {}",
            "✓ passed:".green(),
            passed.to_string().green().bold()
        );
    }
    if failed > 0 {
        println!(
            "  {} {}",
            "✗ failed:".red(),
            failed.to_string().red().bold()
        );
    }
    if errors > 0 {
        println!(
            "  {} {}",
            "E errors:".yellow(),
            errors.to_string().yellow().bold()
        );
    }
    if skipped_run > 0 {
        println!(
            "  {} {}",
            "s skipped (mark):".dimmed(),
            skipped_run.to_string().dimmed()
        );
    }
    if !skipped_tests.is_empty() {
        println!(
            "  {} {} {}",
            "⚡ skipped (unchanged):".cyan(),
            skipped_tests.len().to_string().cyan().bold(),
            "(impact analysis)".dimmed()
        );
    }
    println!("  {} {:.2}s", "time:".dimmed(), elapsed.as_secs_f64());

    // Print failures in detail
    let failures: Vec<&TestResult> = results
        .iter()
        .filter(|r| r.status == TestStatus::Failed || r.status == TestStatus::Error)
        .collect();

    if !failures.is_empty() {
        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
        );
        println!("  {}", "Failures".red().bold());
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
        );
        for f in failures {
            println!();
            println!("  {} {}", "FAILED".red().bold(), f.test_id.bold());
            if let Some(stdout) = &f.stdout {
                let relevant: Vec<&str> = stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(20)
                    .collect();
                for line in relevant {
                    println!("    {}", line.dimmed());
                }
            }
        }
    }

    // Coverage report
    if let Some(cov) = coverage {
        print_coverage_report(cov);
    }

    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );

    // Final status line
    if failed == 0 && errors == 0 {
        println!(
            "  {} {}",
            "✓ All tests passed".green().bold(),
            format!(
                "({} run, {} skipped by impact analysis)",
                passed,
                skipped_tests.len()
            )
            .dimmed()
        );
    } else {
        println!(
            "  {} — {} failed, {} passed",
            "✗ Tests failed".red().bold(),
            failed + errors,
            passed
        );
    }
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
}

fn print_coverage_report(coverage: &HashMap<String, CoverageInfo>) {
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );
    println!("  {}", "Coverage".bold());
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    );

    let mut files: Vec<(&String, &CoverageInfo)> = coverage.iter().collect();
    files.sort_by(|a, b| a.0.cmp(b.0));

    let total_executed: u32 = coverage.values().map(|c| c.executed_lines).sum();
    let total_lines: u32 = coverage.values().map(|c| c.total_lines).sum();
    let overall_pct = if total_lines > 0 {
        (total_executed as f64 / total_lines as f64) * 100.0
    } else {
        100.0
    };

    for (file, info) in &files {
        // Skip stdlib/venv files
        if file.contains("/lib/python") || file.contains(".venv") || file.contains("site-packages")
        {
            continue;
        }
        let bar = coverage_bar(info.percentage);
        let pct_str = format!("{:.0}%", info.percentage);
        let pct_colored = colorize_pct(&pct_str, info.percentage);
        println!(
            "  {} {} {} {}/{}",
            file.dimmed(),
            bar,
            pct_colored,
            info.executed_lines,
            info.total_lines
        );
    }

    println!();
    let overall_str = format!("{:.1}%", overall_pct);
    println!(
        "  {} {}",
        "Overall:".bold(),
        colorize_pct(&overall_str, overall_pct).bold()
    );
}

fn coverage_bar(pct: f64) -> String {
    let filled = ((pct / 100.0) * 10.0).round() as usize;
    let empty = 10 - filled.min(10);
    let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));
    if pct >= 80.0 {
        bar.green().to_string()
    } else if pct >= 60.0 {
        bar.yellow().to_string()
    } else {
        bar.red().to_string()
    }
}

fn colorize_pct(s: &str, pct: f64) -> colored::ColoredString {
    if pct >= 80.0 {
        s.green()
    } else if pct >= 60.0 {
        s.yellow()
    } else {
        s.red()
    }
}
