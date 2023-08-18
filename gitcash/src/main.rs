use std::{path::PathBuf, sync::Arc};

use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use config::Config;
use inquire::{
    validator::{ErrorMessage, Validation},
    Autocomplete,
};
use libgitcash::{Account, AccountType, Repo, Transaction};
use tracing::metadata::LevelFilter;

mod config;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List all accounts
    Accounts,
    /// List all account balances
    Balances,
    /// List all user accounts with negative balances
    Shame,

    /// Interactive CLI
    Cli,
}

#[derive(Clone)]
struct CommandSuggester {
    commands: Vec<&'static str>,
}

impl CommandSuggester {
    pub fn new(commands: &[CliCommand]) -> Self {
        Self {
            commands: commands
                .iter()
                .map(|command| (*command).into())
                .collect::<Vec<&'static str>>(),
        }
    }
}

impl Autocomplete for CommandSuggester {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, inquire::CustomUserError> {
        Ok(self
            .commands
            .iter()
            .filter(|acc| acc.to_lowercase().contains(&input.to_lowercase()))
            .map(|value| value.to_string())
            .collect::<Vec<_>>())
    }

    fn get_completion(
        &mut self,
        _input: &str,
        highlighted_suggestion: Option<String>,
    ) -> Result<inquire::autocompletion::Replacement, inquire::CustomUserError> {
        Ok(highlighted_suggestion)
    }
}

#[derive(Debug, Clone, Copy)]
enum CliCommand {
    AddUser,
    Help,
}

impl Into<&'static str> for CliCommand {
    fn into(self) -> &'static str {
        match self {
            CliCommand::AddUser => "adduser",
            CliCommand::Help => "help",
        }
    }
}

impl TryFrom<&str> for CliCommand {
    type Error = anyhow::Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_ref() {
            "adduser" => Ok(CliCommand::AddUser),
            "help" => Ok(CliCommand::Help),
            other => Err(anyhow!("Invalid command: {}", other)),
        }
    }
}

pub fn main() -> anyhow::Result<()> {
    // Initialize logging subscriber
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(LevelFilter::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set tracing subscriber");

    // Parse args
    let args = Args::parse();

    // Parse config
    let config = Config::load(&args.config)?;

    // Open repo
    let repo = Repo::open(&config.repo_path)?;

    // Run command
    match args.command {
        Command::Accounts => {
            println!("Accounts:");
            for account in repo.accounts() {
                println!("- Account: {} ({:?})", account.name, account.account_type);
            }
        }
        Command::Balances => {
            println!("Balances:");
            for (account, balance) in repo.balances() {
                println!(
                    "- {}: {:.2} CHF [{:?}]",
                    account.name,
                    balance as f32 / 100.0,
                    account.account_type
                );
            }
        }
        Command::Shame => {
            println!("Wall of shame (negative user balances):");
            let negative_balance_accounts = repo
                .balances()
                .into_iter()
                .filter(|(account, balance)| {
                    account.account_type == AccountType::User && *balance < 0
                })
                .collect::<Vec<_>>();
            for (account, balance) in &negative_balance_accounts {
                println!(
                    "- {}: {:.2} CHF [{:?}]",
                    account.name,
                    *balance as f32 / 100.0,
                    account.account_type
                );
            }
            if negative_balance_accounts.is_empty() {
                println!("None at all! 🎉");
            }
        }
        Command::Cli => {
            println!("Welcome to the GitCash CLI for {}!", config.git_name);

            // Get list of valid user account names
            let usernames = Arc::new(
                repo.accounts()
                    .into_iter()
                    .filter(|acc| acc.account_type == AccountType::User)
                    .map(|acc| acc.name)
                    .collect::<Vec<_>>(),
            );

            // Validators
            let existing_username_validator = {
                let usernames = usernames.clone();
                move |value: &str| {
                    Ok(if usernames.iter().any(|name| name == value) {
                        Validation::Valid
                    } else {
                        Validation::Invalid(ErrorMessage::Custom(format!(
                            "Not a known username: {}",
                            value
                        )))
                    })
                }
            };
            let new_username_validator = {
                let usernames = usernames.clone();
                move |value: &str| {
                    let value = value.trim();
                    Ok(if value.is_empty() {
                        Validation::Invalid(ErrorMessage::Custom(
                            "Username may not be empty".into(),
                        ))
                    } else if value.contains(' ') {
                        Validation::Invalid(ErrorMessage::Custom(
                            "Username may not contain a space".into(),
                        ))
                    } else if value.contains(':') {
                        Validation::Invalid(ErrorMessage::Custom(
                            "Username may not contain a colon".into(),
                        ))
                    } else if usernames.iter().any(|name| name == value) {
                        Validation::Invalid(ErrorMessage::Custom(format!(
                            "Username already exists: {}",
                            value
                        )))
                    } else {
                        Validation::Valid
                    })
                }
            };

            // Autocompletion: All names that contain the current input as
            // substring (case-insensitive)
            let name_suggester = {
                move |val: &str| {
                    Ok(usernames
                        .iter()
                        .filter(|acc| acc.to_lowercase().contains(&val.to_lowercase()))
                        .cloned()
                        .collect::<Vec<_>>())
                }
            };

            // Valid commands
            let commands = [CliCommand::AddUser, CliCommand::Help];

            loop {
                // First, ask for command, product or amount
                let target = inquire::Text::new("Amount, EAN or command:")
                    .with_placeholder("e.g. 2.50 CHF")
                    .with_autocomplete(CommandSuggester::new(&commands))
                    .prompt()?;

                // Check whether it's a command
                match CliCommand::try_from(&*target) {
                    Ok(CliCommand::AddUser) => {
                        println!("Adding user");
                        let new_name = inquire::Text::new("Name:")
                            .with_validator(new_username_validator.clone())
                            .prompt()?;
                        let description = format!("Create user {}", new_name);
                        repo.create_transaction(&Transaction {
                            from: Account::source("cash")?,
                            to: Account::user(new_name.clone())?,
                            amount: 0,
                            description: Some(description),
                            meta: None,
                        })?;
                        println!("Successfully added user {}", new_name);
                        continue;
                    }
                    Ok(CliCommand::Help) => {
                        println!("Help");
                        continue;
                    }
                    Err(_) => {}
                };

                // Not a command, treat it as amount
                let amount: f32 = target
                    .parse()
                    .context(format!("Invalid amount: {}", target))?;
                let name = inquire::Text::new("Name:")
                    .with_autocomplete(name_suggester.clone())
                    .with_validator(existing_username_validator.clone())
                    .prompt()?;
                println!("Creating transaction: {} pays {:.2} CHF", name, amount);
                repo.create_transaction(&Transaction {
                    from: Account::user(name)?,
                    to: config.account.clone(),
                    amount: repo.convert_amount(amount),
                    description: None,
                    meta: None,
                })?;
            }
        }
    }

    Ok(())
}
