use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "optimize-prefilter",
    version,
    about = "Generate exact-selectivity CONTAINS suites for prefilter experiments"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover candidates, compute exact truth, and write suites and reports.
    Generate {
        /// Experiment configuration. Relative paths are resolved beside this file.
        #[arg(long, default_value = "experiments/optimize_prefilter/config.toml")]
        config: PathBuf,
        /// Named profile from config.toml; defaults to `default_profile`.
        #[arg(long)]
        profile: Option<String>,
        /// Selection seed. The expensive candidate cache is shared across seeds.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Canonical dataset directory; repeat to override the configured roster.
        #[arg(long = "dataset")]
        datasets: Vec<PathBuf>,
        /// Replace an existing generated seed directory.
        #[arg(long)]
        force: bool,
        /// Ignore compatible per-length catalogue files and discover them again.
        #[arg(long)]
        rebuild_cache: bool,
    },
    /// List configured workload profiles and their exact scopes.
    Profiles {
        /// Experiment configuration.
        #[arg(long, default_value = "experiments/optimize_prefilter/config.toml")]
        config: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Generate {
            config,
            profile,
            seed,
            datasets,
            force,
            rebuild_cache,
        } => {
            let cfg = optimize_prefilter::Config::load(&config)?;
            let outcome = optimize_prefilter::generate_experiment(
                &cfg,
                profile.as_deref(),
                seed,
                &datasets,
                force,
                rebuild_cache,
            )?;
            println!(
                "generated {} queries across {} dataset(s) in {}",
                outcome.queries,
                outcome.datasets,
                outcome.seed_dir.display()
            );
            println!(
                "benchmark spec: {}",
                outcome.seed_dir.join("benchmark.toml").display()
            );
            println!(
                "next: ./experiments/optimize_prefilter/benchmark.sh --profile {} --seed {}",
                outcome.profile, seed
            );
        }
        Command::Profiles { config } => {
            let cfg = optimize_prefilter::Config::load(&config)?;
            for (name, profile) in &cfg.profiles {
                let marker = if name == &cfg.default_profile {
                    " (default)"
                } else {
                    ""
                };
                let scope = profile
                    .max_selectivity_percent
                    .map(|pct| format!("selectivity < {pct}%"))
                    .unwrap_or_else(|| "full selectivity space".to_string());
                println!(
                    "{name}{marker}: {scope}; replicas zero/low/other={}/{}/{}\n  {}",
                    profile.zero_replicates,
                    profile.low_selectivity_replicates,
                    profile.other_replicates,
                    profile.description
                );
            }
        }
    }
    Ok(())
}
