mod collector;
mod config;
mod db;
mod hasher;
mod impact;
mod reporter;
mod runner;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use std::time::Instant;

use config::{RiptideConfig, DEFAULT_DB, DEFAULT_PATTERN, DEFAULT_TIMEOUT_SECS};

#[derive(Parser)]
#[command(
    name = "riptide",
    about = "⚡ Rust-powered Python test engine — parallel execution, impact analysis, coverage",
    version = "0.1.0"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Test paths to run (files or directories) [default: tests test]
    paths: Vec<PathBuf>,

    /// Number of parallel workers [default: CPU count]
    #[arg(short = 'n', long, help = "Workers (0 = CPU count)")]
    workers: Option<usize>,

    /// Python binary to use [default: python3]
    #[arg(long)]
    python: Option<String>,

    /// Enable coverage measurement
    #[arg(long, short = 'c')]
    coverage: bool,

    /// Ignore impact analysis — run all tests
    #[arg(long)]
    all: bool,

    /// Run one pytest process per test (legacy isolation; slower cold start)
    #[arg(long)]
    isolate: bool,

    /// File name pattern for test discovery
    #[arg(long)]
    pattern: Option<String>,

    /// Path to state database [default: .riptide.db]
    #[arg(long)]
    db: Option<PathBuf>,

    /// Per-test timeout in seconds [default: 300]
    #[arg(long)]
    timeout: Option<u64>,
}

#[derive(Subcommand)]
enum Commands {
    /// Collect and list all tests without running
    Collect {
        paths: Vec<PathBuf>,
        #[arg(long)]
        pattern: Option<String>,
    },
    /// Clear the state database (forces full re-run next time)
    Clear {
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Show coverage report from last run
    Coverage,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // pyproject.toml in the current directory provides defaults below the CLI.
    let cfg = RiptideConfig::load(&PathBuf::from("pyproject.toml"))?;

    match &cli.command {
        Some(Commands::Collect { paths, pattern }) => {
            let pattern = pattern
                .clone()
                .or_else(|| cfg.pattern.clone())
                .unwrap_or_else(|| DEFAULT_PATTERN.to_string());
            let paths = resolve_paths(paths, &cfg);
            cmd_collect(&paths, &pattern)
        }
        Some(Commands::Clear { db }) => {
            let db = resolve_db(db.clone(), &cfg);
            cmd_clear(&db)
        }
        Some(Commands::Coverage) => {
            let python = resolve_python(&cli.python, &cfg);
            cmd_coverage(&python)
        }
        None => cmd_run(&cli, &cfg),
    }
}

fn resolve_python(cli: &Option<String>, cfg: &RiptideConfig) -> String {
    cli.clone()
        .or_else(|| cfg.python.clone())
        .unwrap_or_else(|| "python3".to_string())
}

fn resolve_db(cli: Option<PathBuf>, cfg: &RiptideConfig) -> PathBuf {
    cli.or_else(|| cfg.db.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DB))
}

fn resolve_paths(cli: &[PathBuf], cfg: &RiptideConfig) -> Vec<PathBuf> {
    if !cli.is_empty() {
        return cli.to_vec();
    }
    cfg.paths
        .clone()
        .unwrap_or_else(|| vec![PathBuf::from("tests"), PathBuf::from("test")])
}

fn cmd_run(cli: &Cli, cfg: &RiptideConfig) -> Result<()> {
    // Resolve effective settings: explicit CLI flag > pyproject > built-in default.
    let python = resolve_python(&cli.python, cfg);
    let db_path = resolve_db(cli.db.clone(), cfg);
    let pattern = cli
        .pattern
        .clone()
        .or_else(|| cfg.pattern.clone())
        .unwrap_or_else(|| DEFAULT_PATTERN.to_string());
    let with_coverage = cli.coverage || cfg.coverage.unwrap_or(false);
    let isolate = cli.isolate || cfg.isolate.unwrap_or(false);
    let timeout_secs = cli.timeout.or(cfg.timeout).unwrap_or(DEFAULT_TIMEOUT_SECS);
    let workers = match cli.workers.or(cfg.workers) {
        Some(0) | None => num_cpus(),
        Some(n) => n,
    };

    let requested = resolve_paths(&cli.paths, cfg);
    let paths: Vec<PathBuf> = requested.iter().filter(|p| p.exists()).cloned().collect();
    if paths.is_empty() {
        eprintln!(
            "{} No test paths found. Tried: {:?}",
            "error:".red().bold(),
            requested
        );
        std::process::exit(1);
    }

    let db = db::Database::open(&db_path)?;

    print!("  {} collecting tests...", "⟳".cyan());
    let all_tests = collector::collect_tests(&paths, &pattern)?;
    println!("\r  {} collected {} tests", "✓".green(), all_tests.len());

    if all_tests.is_empty() {
        println!("  {} No tests found in {:?}", "!".yellow(), paths);
        return Ok(());
    }

    let current_hashes = {
        let mut all = std::collections::HashMap::new();
        for path in &paths {
            all.extend(hasher::hash_all_python_files(path)?);
        }
        if let Ok(h) = hasher::hash_all_python_files(&PathBuf::from(".")) {
            all.extend(h);
        }
        all
    };

    let (to_run, skipped_tests) = if cli.all {
        println!(
            "  {} --all flag: running all {} tests",
            "!".yellow(),
            all_tests.len()
        );
        (all_tests.clone(), vec![])
    } else {
        let changed_files = hasher::find_changed_files(&current_hashes, &db)?;
        if changed_files.is_empty() {
            println!("  {} no files changed", "⚡".cyan());
        } else {
            println!("  {} {} file(s) changed:", "⚡".cyan(), changed_files.len());
            for f in changed_files.iter().take(5) {
                println!("    {}", f.dimmed());
            }
            if changed_files.len() > 5 {
                println!("    {} more...", changed_files.len() - 5);
            }
        }
        let analyzer = impact::ImpactAnalyzer::new(&db, changed_files, &all_tests);
        analyzer.filter_affected(&all_tests)?
    };

    reporter::print_header(to_run.len(), skipped_tests.len(), workers, with_coverage);

    if to_run.is_empty() {
        println!(
            "  {} All tests skipped — no changes detected!",
            "⚡".cyan().bold()
        );
        println!(
            "  {} Use {} to force a full run.",
            "tip:".dimmed(),
            "--all".bold()
        );
        return Ok(());
    }

    let runner = runner::Runner::new(workers, &python, with_coverage, timeout_secs, isolate);
    let start = Instant::now();
    let results = runner.run_parallel(&to_run)?;
    let elapsed = start.elapsed();

    for result in &results {
        db.save_test_result(result)?;
        if !result.covered_files.is_empty() {
            db.save_test_deps(&result.test_id, &result.covered_files)?;
        }
    }

    hasher::save_hashes(&current_hashes, &db)?;

    let coverage_report = if with_coverage {
        match runner::merge_coverage(&python, &runner.coverage_dir) {
            Ok(cov) => {
                // W3: persist the coverage report so history is queryable.
                let run_id = chrono::Utc::now().to_rfc3339();
                if let Err(e) = db.save_coverage(&run_id, &cov) {
                    eprintln!("  {} could not persist coverage: {}", "warn:".yellow(), e);
                }
                Some(cov)
            }
            Err(e) => {
                eprintln!("  {} coverage merge failed: {}", "warn:".yellow(), e);
                None
            }
        }
    } else {
        None
    };

    reporter::print_summary(&results, &skipped_tests, elapsed, coverage_report.as_ref());

    let failed = results
        .iter()
        .any(|r| r.status == runner::TestStatus::Failed || r.status == runner::TestStatus::Error);
    if failed {
        std::process::exit(1);
    }

    Ok(())
}

fn cmd_collect(paths: &[PathBuf], pattern: &str) -> Result<()> {
    let existing: Vec<PathBuf> = paths.iter().filter(|p| p.exists()).cloned().collect();
    let existing = if existing.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        existing
    };
    let tests = collector::collect_tests(&existing, pattern)?;
    println!("  {} {} tests collected:", "✓".green(), tests.len());
    for t in &tests {
        println!("  {}", t.test_id.dimmed());
    }
    Ok(())
}

fn cmd_clear(db_path: &PathBuf) -> Result<()> {
    if db_path.exists() {
        std::fs::remove_file(db_path)?;
        println!(
            "  {} State database cleared. Next run will execute all tests.",
            "✓".green()
        );
    } else {
        println!("  {} No database found at {:?}", "!".yellow(), db_path);
    }
    Ok(())
}

fn cmd_coverage(python_bin: &str) -> Result<()> {
    let cov_dir = PathBuf::from(".riptide-coverage");
    if !cov_dir.exists() {
        println!(
            "  {} No coverage data found. Run with {} first.",
            "!".yellow(),
            "--coverage".bold()
        );
        return Ok(());
    }
    match runner::merge_coverage(python_bin, &cov_dir) {
        Ok(cov) => {
            let dummy_results: Vec<runner::TestResult> = vec![];
            let dummy_skipped: Vec<collector::TestItem> = vec![];
            reporter::print_summary(
                &dummy_results,
                &dummy_skipped,
                std::time::Duration::ZERO,
                Some(&cov),
            );
        }
        Err(e) => eprintln!("  {} {}", "error:".red(), e),
    }
    Ok(())
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
