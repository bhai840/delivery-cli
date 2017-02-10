//
// Copyright:: Copyright (c) 2016 Chef Software, Inc.
// License:: Apache License, Version 2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

use std;
use std::process;
use std::time::Duration;
use std::path::PathBuf;
use std::path::Path;
use std::io::prelude::*;
use std::fs::File;
use utils;
use utils::say::{self, sayln};
use errors::{Kind, DeliveryError};
use types::{DeliveryResult, ExitCode};
use config::Config;
use clap::{App, ArgMatches, AppSettings};
use project;
use git;
use delivery_config::project::ProjectToml;
use utils::cwd;

// Clap Arguments
//
// Encapsulated ClapArguments that will be share across commands including
// the ClapAlias trait for arguments that we might depricate in the future
#[macro_use]
pub mod arguments;
use cli::arguments::{non_interactive_arg, no_spinner_arg};

// Modules for setting up clap subcommand including their options and defaults,
// as well as advanced subcommand match parsing (see local for an example).
pub mod api;
pub mod review;
pub mod checkout;
pub mod clone;
pub mod diff;
pub mod init;
pub mod job;
pub mod token;
pub mod setup;
pub mod local;
mod spin;

// Implemented sub-commands. Should handle everything after args have
// been parsed, including running the command, error handling, and UI outputting.
use command::Command;
use command::api::ApiCommand;
use command::checkout::CheckoutCommand;
use command::clone::CloneCommand;
use command::diff::DiffCommand;
use command::init::InitCommand;
use command::job::{JobCommand, run_docker_job};
use command::local::LocalCommand;
use command::review::ReviewCommand;
use command::setup::SetupCommand;
use command::token::TokenCommand;

pub trait CommandPrep {
    fn merge_options_and_config(&self, config: Config) -> DeliveryResult<Config>;

    // The initialization of a CLI command could be different from another one
    // so we need a main method we can easily override if such behavior is
    // different. This fun will provide an easy way to do so by calling other
    // specific initialization functions, this will allow you to take actions
    // after or before making a call.
    //
    // You can also override this to fix the initialization needs of your command
    // (for example, simply return the config in a non-project specific command
    // like api).
    //
    // By default we call the project specific commands
    fn initialize_command_state(&self, config: Config) -> DeliveryResult<Config> {
        self.init_project_specific(config)
    }

    // Project specific commands.
    //
    // Most project specific commands need to populate the project config entry
    // if it hasn't been already as well as make sure the git remote is up to date.
    fn init_project_specific(&self, mut config: Config) -> DeliveryResult<Config> {
        if config.project.is_none() {
            config.project = project::project_from_cwd().ok();
        }

        let git_url = try!(config.delivery_git_ssh_url());
        try!(git::create_or_update_delivery_remote(&git_url, &try!(project::project_path())));
        if config.fips.unwrap_or(false) {
            if !Path::new("/opt/chefdk/embedded/bin/stunnel").exists() {
                return Err(DeliveryError{ kind: Kind::FipsNotSupportedForChefDKPlatform, detail: None })
            }
            try!(std::fs::create_dir_all(try!(utils::home_dir(&[".chefdk/etc/"]))));
            try!(std::fs::create_dir_all(try!(utils::home_dir(&[".chefdk/log/"]))));

            let mut conf_file = try!(File::create(try!(utils::home_dir(&[".chefdk/etc/stunnel.conf"]))));
            try!(conf_file.write_all(b"fips = yes\n"));
            try!(conf_file.write_all(b"client = yes\n"));

            let output = "output = ".to_string();
            let output_conf = output + try!(utils::home_dir(&[".chefdk/log/stunnel.log\n"])).to_str().unwrap();
            try!(conf_file.write_all(output_conf.as_bytes()));

            try!(conf_file.write_all(b"[git]\n"));

            let fips_git_port = config.fips_git_port.as_ref().unwrap();
            let accept = "accept = ".to_string() + fips_git_port + "\n";
            try!(conf_file.write_all(accept.as_bytes()));

            let server = config.server.as_ref().unwrap();
            let connect = "connect = ".to_string() + server + ":8989\n";
            try!(conf_file.write_all(connect.as_bytes()));

            let ca_file = "CAfile = ".to_string() + try!(utils::home_dir(&[".chefdk/etc/automate-nginx-cert.pem\n"])).to_str().unwrap();
            try!(conf_file.write_all(ca_file.as_bytes()));

            try!(conf_file.write_all(b"verifyChain = yes\n"));

            let check_host = "checkHost = ".to_string() + server +"\n";
            try!(conf_file.write_all(check_host.as_bytes()));
        }

        Ok(config)
    }
}

pub fn run() {
    let build_version = format!("{} {}", version(), build_git_sha());

    let app = make_app(&build_version);
    let app_matches = app.get_matches();

    match match_command_and_start(&app_matches, &build_version) {
        // You can exit with any integer, can also be used to bypass default
        // error handling if you handled an error and returned non-zero.
        Ok(exit_status) => process::exit(exit_status),
        // Handles DeliveryError and exits 1.
        Err(e) => exit_with(e, 1)
    }
}

fn execute_command<C: Command>(matches: &ArgMatches, command: C) -> DeliveryResult<ExitCode> {
    handle_spinner(&matches);
    command.run()
}

fn match_command_and_start(app_matches: &ArgMatches, build_version: &str) -> DeliveryResult<ExitCode> {
    let cmd_result = match app_matches.subcommand() {
        (api::SUBCOMMAND_NAME, Some(matches)) => {
            let options = api::ApiClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = ApiCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (checkout::SUBCOMMAND_NAME, Some(matches)) => {
            let options = checkout::CheckoutClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = CheckoutCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (clone::SUBCOMMAND_NAME, Some(matches)) => {
            let options = clone::CloneClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = CloneCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (diff::SUBCOMMAND_NAME, Some(matches)) => {
            let options = diff::DiffClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = DiffCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (init::SUBCOMMAND_NAME, Some(matches)) => {
            let options = init::InitClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = InitCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (job::SUBCOMMAND_NAME, Some(matches)) => {
            let options = job::JobClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = JobCommand{options: &options, config: &config};
            if !options.docker_image.is_empty() {
                run_docker_job(&options)
            } else {
                execute_command(&matches, command)
            }
        },
        (local::SUBCOMMAND_NAME, Some(matches)) => {
            let options = local::LocalClapOptions::new(&matches);
            let config = try!(ProjectToml::load_toml(options.remote_toml));
            let command = LocalCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (review::SUBCOMMAND_NAME, Some(matches)) => {
            let options = review::ReviewClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = ReviewCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (setup::SUBCOMMAND_NAME, Some(matches)) => {
            let options = setup::SetupClapOptions::new(&matches);
            let config_path = if options.path.is_empty() {
                cwd()
            } else {
                PathBuf::from(options.path)
            };
            let config = try!(init_command(&options));
            let command = SetupCommand{
                options: &options,
                config: &config,
                config_path: &config_path,
            };
            execute_command(&matches, command)
        },
        (token::SUBCOMMAND_NAME, Some(matches)) => {
            let options = token::TokenClapOptions::new(&matches);
            let config = try!(init_command(&options));
            let command = TokenCommand{options: &options, config: &config};
            execute_command(&matches, command)
        },
        (spin::SUBCOMMAND_NAME, Some(matches)) => {
            handle_spinner(&matches);
            let spin_opts = spin::SpinClapOptions::new(&matches);
            let spinner = utils::say::Spinner::start();
            let sleep_time = Duration::from_secs(spin_opts.time);
            std::thread::sleep(sleep_time);
            spinner.stop();
            handle_spinner(&matches);
            Ok(0)
        },
        _ => {
            // ownership issue with use of above defined app
            // so for now...
            let mut a = make_app(&build_version);
            a.print_help().ok().expect("failed to write help to stdout");
            Ok(1)
        }
    };
    cmd_result
}

pub fn merge_fips_options_and_config(fips: bool, fips_git_port: &str, mut config: Config) -> DeliveryResult<Config> {
    if config.fips.is_none() {
        config.fips = Some(fips);
    }

    let new_config = config.set_fips_git_port(fips_git_port);

    Ok(new_config)
}

fn make_app<'a>(version: &'a str) -> App<'a, 'a> {
    App::new("delivery")
        .version(version)
        .setting(AppSettings::GlobalVersion)
        .arg(no_spinner_arg().global(true))
        .arg(non_interactive_arg().global(true))
        .subcommand(review::clap_subcommand())
        .subcommand(clone::clap_subcommand())
        .subcommand(checkout::clap_subcommand())
        .subcommand(diff::clap_subcommand())
        .subcommand(init::clap_subcommand())
        .subcommand(setup::clap_subcommand())
        .subcommand(job::clap_subcommand())
        .subcommand(api::clap_subcommand())
        .subcommand(token::clap_subcommand())
        .subcommand(spin::clap_subcommand())
        .subcommand(local::clap_subcommand())
}

fn handle_spinner(matches: &ArgMatches) {
    if matches.is_present("no-spinner") {
        say::turn_off_spinner()
    }
}

fn exit_with(e: DeliveryError, i: ExitCode) {
    sayln("red", &format!("{}", e));
    if let Some(dtail) = e.detail() {
        sayln("red", &dtail);
    }
    process::exit(i)
}

fn init_command<T: CommandPrep>(opts: &T) -> DeliveryResult<Config> {
    let mut config = try!(Config::load_config(&utils::cwd()));
    debug!("Initial config: {:?}", config);
    config = try!(opts.merge_options_and_config(config));
    debug!("Merged config: {:?}", config);
    config = try!(opts.initialize_command_state(config));
    debug!("Command specific config: {:?}", config);
    Ok(config)
}

fn version() -> String {
    let build_version = option_env!("DELIV_CLI_VERSION").unwrap_or("0.0.0");
    format!("{}", build_version)
}

fn build_git_sha() -> String {
    let sha = option_env!("DELIV_CLI_GIT_SHA").unwrap_or("0000");
    format!("({})", sha)
}

#[cfg(test)]
mod tests {
    use cli;
    use cli::{api, review, clone, checkout, diff, init, job, spin, token, setup};

    #[test]
    fn test_clap_api_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "api", "get", "endpoint",
                                           "--data", "\"name\":\"n\",\"value\":\"d\"",
                                           "-e", "starwars", "-u", "vader", "-s",
                                           "death-star", "--api-port", "9999"]);
        assert_eq!(Some("api"), matches.subcommand_name());
        let api_matches = matches.subcommand_matches(api::SUBCOMMAND_NAME).unwrap();
        let api_opts = api::ApiClapOptions::new(&api_matches);
        assert_eq!(api_opts.method, "get");
        assert_eq!(api_opts.path, "endpoint");
        assert_eq!(api_opts.data, "\"name\":\"n\",\"value\":\"d\"");
        assert_eq!(api_opts.server, "death-star");
        assert_eq!(api_opts.api_port, "9999");
        assert_eq!(api_opts.ent, "starwars");
        assert_eq!(api_opts.user, "vader");
    }

    #[test]
    fn test_clap_review_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "review", "--auto-bump",
                                           "--no-open", "--edit", "-f", "custom-pipe"]);
        assert_eq!(Some("review"), matches.subcommand_name());
        let review_matches = matches.subcommand_matches(review::SUBCOMMAND_NAME).unwrap();
        let review_opts = review::ReviewClapOptions::new(&review_matches);
        assert_eq!(review_opts.pipeline, "custom-pipe");
        assert_eq!(review_opts.no_open, true);
        assert_eq!(review_opts.auto_bump, true);
        assert_eq!(review_opts.edit, true);
    }

    #[test]
    fn test_clap_checkout_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "checkout", "change_the_force",
                                           "-P", "p4tchs3t", "-f", "custom-pipe"]);
        assert_eq!(Some("checkout"), matches.subcommand_name());
        let checkout_matches = matches.subcommand_matches(checkout::SUBCOMMAND_NAME).unwrap();
        let checkout_opts = checkout::CheckoutClapOptions::new(&checkout_matches);
        assert_eq!(checkout_opts.pipeline, "custom-pipe");
        assert_eq!(checkout_opts.change, "change_the_force");
        assert_eq!(checkout_opts.patchset, "p4tchs3t");
    }

    #[test]
    fn test_clap_clone_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "clone", "minecraft",
                                           "-e", "world", "-o", "coolest", "-u",
                                           "dummy", "-s", "m.craft.com", "-g",
                                           "ssh://another.world.com:123/awesome"]);
        assert_eq!(Some("clone"), matches.subcommand_name());
        let clone_matches = matches.subcommand_matches(clone::SUBCOMMAND_NAME).unwrap();
        let clone_opts = clone::CloneClapOptions::new(&clone_matches);
        assert_eq!(clone_opts.project, "minecraft");
        assert_eq!(clone_opts.user, "dummy");
        assert_eq!(clone_opts.server, "m.craft.com");
        assert_eq!(clone_opts.ent, "world");
        assert_eq!(clone_opts.org, "coolest");
        assert_eq!(clone_opts.git_url, "ssh://another.world.com:123/awesome");
    }

    #[test]
    fn test_clap_diff_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "diff", "change-me", "-l",
                                           "-P", "p4tchs3t", "-f", "coolest"]);
        assert_eq!(Some("diff"), matches.subcommand_name());
        let diff_matches = matches.subcommand_matches(diff::SUBCOMMAND_NAME).unwrap();
        let diff_opts = diff::DiffClapOptions::new(&diff_matches);
        assert_eq!(diff_opts.change, "change-me");
        assert_eq!(diff_opts.patchset, "p4tchs3t");
        assert_eq!(diff_opts.pipeline, "coolest");
        assert_eq!(diff_opts.local, true);
    }

    #[test]
    fn test_clap_init_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let init_cmd = vec!["delivery", "init", "-l", "-p", "frijol", "-u", "concha",
                        "-s", "cocina.central.com", "-e", "mexicana", "-o", "oaxaca",
                        "-f", "postres", "-c", "receta.json", "--generator", "/original",
                        "--github", "git-mx", "--bitbucket", "bit-mx", "-r", "antojitos",
                        "--no-verify-ssl", "--skip-build-cookbook", "-n"];
        let matches = app.get_matches_from(init_cmd);
        assert_eq!(Some("init"), matches.subcommand_name());
        let init_matches = matches.subcommand_matches(init::SUBCOMMAND_NAME).unwrap();
        let init_opts = init::InitClapOptions::new(&init_matches);
        assert_eq!(init_opts.pipeline, "postres");
        assert_eq!(init_opts.user, "concha");
        assert_eq!(init_opts.server, "cocina.central.com");
        assert_eq!(init_opts.ent, "mexicana");
        assert_eq!(init_opts.org, "oaxaca");
        assert_eq!(init_opts.project, "frijol");
        assert_eq!(init_opts.config_json, "receta.json");
        assert_eq!(init_opts.generator, "/original");
        assert_eq!(init_opts.github_org_name, "git-mx");
        assert_eq!(init_opts.bitbucket_project_key, "bit-mx");
        assert_eq!(init_opts.repo_name, "antojitos");
        assert_eq!(init_opts.no_v_ssl, true);
        assert_eq!(init_opts.no_open, true);
        assert_eq!(init_opts.skip_build_cookbook, true);
        assert_eq!(init_opts.local, true);
    }

    #[test]
    fn test_clap_job_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let job_cmd = vec!["delivery", "job", "anime", "ninja", "-C", "rasengan",
                        "-u", "naruto", "-s", "manga.com", "-e", "shippuden", "-o",
                        "akatsuki", "-f", "sharingan", "-j", "/path", "-p", "uchiha",
                        "-P", "latest", "--change-id", "super-cool-id", "-g", "powerful-url",
                        "-S", "SHA", "-b", "evil", "--skip-default", "-l", "--docker", "uzumaki"];
        let matches = app.get_matches_from(job_cmd);
        assert_eq!(Some("job"), matches.subcommand_name());
        let job_matches = matches.subcommand_matches(job::SUBCOMMAND_NAME).unwrap();
        let job_opts = job::JobClapOptions::new(&job_matches);
        assert_eq!(job_opts.pipeline, "sharingan");
        assert_eq!(job_opts.stage, "anime");
        assert_eq!(job_opts.phases, "ninja");
        assert_eq!(job_opts.user, "naruto");
        assert_eq!(job_opts.server, "manga.com");
        assert_eq!(job_opts.change, "rasengan");
        assert_eq!(job_opts.ent, "shippuden");
        assert_eq!(job_opts.org, "akatsuki");
        assert_eq!(job_opts.job_root, "/path");
        assert_eq!(job_opts.project, "uchiha");
        assert_eq!(job_opts.patchset, "latest");
        assert_eq!(job_opts.change_id, "super-cool-id");
        assert_eq!(job_opts.git_url, "powerful-url");
        assert_eq!(job_opts.shasum, "SHA");
        assert_eq!(job_opts.branch, "evil");
        assert_eq!(job_opts.docker_image, "uzumaki");
        assert_eq!(job_opts.local, true);
        assert_eq!(job_opts.skip_default, true);
    }

    #[test]
    fn test_clap_spin_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "spin"]);
        assert_eq!(Some("spin"), matches.subcommand_name());
        let spin_matches = matches.subcommand_matches(spin::SUBCOMMAND_NAME).unwrap();
        let spin_opts = spin::SpinClapOptions::new(&spin_matches);
        assert_eq!(spin_opts.time, 5);
    }

    #[test]
    fn test_clap_token_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "token", "-e", "fellowship",
                                           "-u", "gandalf", "-s", "lord.of.the.rings.com",
                                           "--api-port", "1111", "--verify", "--saml=true"]);
        assert_eq!(Some("token"), matches.subcommand_name());
        let token_matches = matches.subcommand_matches(token::SUBCOMMAND_NAME).unwrap();
        let token_opts = token::TokenClapOptions::new(&token_matches);
        assert_eq!(token_opts.server, "lord.of.the.rings.com");
        assert_eq!(token_opts.port, "1111");
        assert_eq!(token_opts.ent, "fellowship");
        assert_eq!(token_opts.user, "gandalf");
        assert_eq!(token_opts.verify, true);
        assert_eq!(token_opts.saml, Some(true));
    }

    #[test]
    fn test_clap_setup_options() {
        let build_version = format!("{} {}", cli::version(), cli::build_git_sha());
        let app = cli::make_app(&build_version);
        let matches = app.get_matches_from(vec!["delivery", "setup", "-e", "e", "-u", "u",
                                           "-s", "s", "--config-path", "/my/config/cli.toml",
                                           "-f", "p", "-o", "good"]);
        assert_eq!(Some("setup"), matches.subcommand_name());
        let setup_matches = matches.subcommand_matches(setup::SUBCOMMAND_NAME).unwrap();
        let setup_opts = setup::SetupClapOptions::new(&setup_matches);
        assert_eq!(setup_opts.server, "s");
        assert_eq!(setup_opts.org, "good");
        assert_eq!(setup_opts.ent, "e");
        assert_eq!(setup_opts.user, "u");
        assert_eq!(setup_opts.pipeline, "p");
        assert_eq!(setup_opts.path, "/my/config/cli.toml");
    }
}
