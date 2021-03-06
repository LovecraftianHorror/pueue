use ::std::collections::HashMap;
use ::std::env::{current_dir, vars};
use ::std::io::{self, Write};

use ::anyhow::{bail, Context, Result};
use ::async_std::net::TcpStream;
use ::log::error;

use ::pueue::message::*;
use ::pueue::protocol::*;
use ::pueue::settings::Settings;

use crate::cli::{Opt, SubCommand};
use crate::commands::edit::*;
use crate::commands::local_follow::*;
use crate::commands::restart::*;
use crate::output::*;

/// This struct contains the base logic for the client.
/// The client is responsible for connecting to the daemon, sending instructions
/// and interpreting their responses.
///
/// Most commands are a simple ping-pong. Though, some commands require a more complex
/// communication pattern (e.g. `show -f`, which continuously streams the output of a task).
pub struct Client {
    opt: Opt,
    settings: Settings,
    socket: TcpStream,
}

impl Client {
    pub async fn new(settings: Settings, opt: Opt) -> Result<Self> {
        // // Commandline argument overwrites the configuration files values for address
        // let address = if let Some(address) = opt.address.clone() {
        //     address
        // } else {
        //     settings.client.daemon_address
        // };

        // Commandline argument overwrites the configuration files values for port
        let port = if let Some(port) = opt.port.clone() {
            port
        } else {
            settings.client.daemon_port.clone()
        };

        // Don't allow anything else than loopback until we have proper crypto
        // let address = format!("{}:{}", address, port);
        let address = format!("127.0.0.1:{}", port);

        // Connect to socket
        let mut socket = TcpStream::connect(&address)
            .await
            .context("Failed to connect to the daemon. Did you start it?")?;
        let secret = settings.client.secret.clone().into_bytes();
        send_bytes(secret, &mut socket).await?;

        Ok(Client {
            opt,
            settings,
            socket,
        })
    }

    /// This is the function where the actual communication and logic starts.
    /// At this point everything is initialized, the connection is up and
    /// we can finally start doing stuff.
    ///
    /// The command handling is splitted into "simple" and "complex" commands.
    pub async fn start(&self) -> Result<()> {
        // Return early, if the command has already been handled.
        if self.handle_complex_command().await? {
            return Ok(());
        }

        // The handling of "generic" commands is encapsulated in this function.
        self.handle_simple_command().await?;

        Ok(())
    }

    /// Handle all complex client-side functionalities.
    /// Complex functionalities need some special handling and are contained
    /// in their own functions with their own communication code.
    /// Such functionalities includes reading local files, data streaming
    /// and sending multiple messages.
    ///
    /// Returns `true`, if the current command has been handled by this function.
    /// This indicates that the client can now shut down.
    async fn handle_complex_command(&self) -> Result<bool> {
        let mut socket = self.socket.clone();
        // This match handles all "complex" commands.
        match &self.opt.cmd {
            SubCommand::Edit { task_id, path } => {
                let message = edit(&mut socket, *task_id, *path).await?;
                self.handle_response(message);
                Ok(true)
            }
            SubCommand::Restart {
                task_ids,
                start_immediately,
                stashed,
                edit,
                path,
            } => {
                restart(
                    &mut socket,
                    task_ids.clone(),
                    *start_immediately,
                    *stashed,
                    *edit,
                    *path,
                )
                .await?;
                Ok(true)
            }
            SubCommand::Follow { task_id, err } => {
                // Simple log output follows for local logs don't need any communication with the daemon.
                // Thereby we handle this separately over here.
                if self.settings.client.read_local_logs {
                    local_follow(
                        &mut socket,
                        self.settings.daemon.pueue_directory.clone(),
                        task_id,
                        *err,
                    )
                    .await?;
                    return Ok(true);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Handle logic that's super generic on the client-side.
    /// This always follows a singular ping-pong pattern.
    /// One message to the daemon, one response, Done.
    async fn handle_simple_command(&self) -> Result<()> {
        let mut socket = self.socket.clone();

        // Create the message that should be sent to the daemon
        // depending on the given commandline options.
        let message = self.get_message_from_opt()?;

        // Create the message payload and send it to the daemon.
        send_message(message, &mut socket).await?;

        // Check if we can receive the response from the daemon
        let mut response = receive_message(&mut socket).await?;

        // Check if we can receive the response from the daemon
        while self.handle_response(response) {
            response = receive_message(&mut socket).await?;
        }

        Ok(())
    }

    /// Most returned messages can be handled in a generic fashion.
    /// However, some commands require continuous receiving of messages (streaming).
    ///
    /// If this function returns `Ok(true)`, the parent function will continue to receive
    /// and handle messages from the daemon. Otherwise the client will simply exit.
    fn handle_response(&self, message: Message) -> bool {
        match message {
            Message::Success(text) => print_success(text),
            Message::Failure(text) => print_error(text),
            Message::StatusResponse(state) => print_state(state, &self.opt.cmd),
            Message::LogResponse(task_logs) => print_logs(task_logs, &self.opt.cmd, &self.settings),
            Message::Stream(text) => {
                print!("{}", text);
                io::stdout().flush().unwrap();
                return true;
            }
            _ => error!("Received unhandled response message"),
        };

        false
    }

    /// Convert the cli command into the message that's being sent to the server,
    /// so it can be understood by the daemon.
    fn get_message_from_opt(&self) -> Result<Message> {
        match &self.opt.cmd {
            SubCommand::Add {
                command,
                start_immediately,
                stashed,
                group,
                delay_until,
                dependencies,
            } => {
                let cwd_pathbuf = current_dir()?;
                let cwd = cwd_pathbuf
                    .to_str()
                    .context("Cannot parse current working directory (Invalid utf8?)")?;

                let mut envs = HashMap::new();
                // Save all environment variables for later injection into the started task
                for (key, value) in vars() {
                    envs.insert(key, value);
                }

                Ok(Message::Add(AddMessage {
                    command: command.join(" "),
                    path: cwd.to_string(),
                    envs,
                    start_immediately: *start_immediately,
                    stashed: *stashed,
                    group: group.clone(),
                    enqueue_at: *delay_until,
                    dependencies: dependencies.to_vec(),
                    ignore_aliases: false,
                }))
            }
            SubCommand::Remove { task_ids } => Ok(Message::Remove(task_ids.clone())),
            SubCommand::Stash { task_ids } => Ok(Message::Stash(task_ids.clone())),
            SubCommand::Switch {
                task_id_1,
                task_id_2,
            } => {
                let message = SwitchMessage {
                    task_id_1: *task_id_1,
                    task_id_2: *task_id_2,
                };
                Ok(Message::Switch(message))
            }
            SubCommand::Enqueue {
                task_ids,
                delay_until,
            } => {
                let message = EnqueueMessage {
                    task_ids: task_ids.clone(),
                    enqueue_at: *delay_until,
                };
                Ok(Message::Enqueue(message))
            }
            SubCommand::Start {
                task_ids,
                group,
                all,
                children,
            } => {
                let message = StartMessage {
                    task_ids: task_ids.clone(),
                    group: group.clone(),
                    all: *all,
                    children: *children,
                };
                Ok(Message::Start(message))
            }
            SubCommand::Pause {
                task_ids,
                group,
                wait,
                all,
                children,
            } => {
                let message = PauseMessage {
                    task_ids: task_ids.clone(),
                    group: group.clone(),
                    wait: *wait,
                    all: *all,
                    children: *children,
                };
                Ok(Message::Pause(message))
            }
            SubCommand::Kill {
                task_ids,
                group,
                default,
                all,
                children,
            } => {
                let message = KillMessage {
                    task_ids: task_ids.clone(),
                    group: group.clone(),
                    default: *default,
                    all: *all,
                    children: *children,
                };
                Ok(Message::Kill(message))
            }
            SubCommand::Send { task_id, input } => {
                let message = SendMessage {
                    task_id: *task_id,
                    input: input.clone(),
                };
                Ok(Message::Send(message))
            }
            SubCommand::Group { add, remove } => {
                let message = GroupMessage {
                    add: add.clone(),
                    remove: remove.clone(),
                };
                Ok(Message::Group(message))
            }
            SubCommand::Status { .. } => Ok(Message::Status),
            SubCommand::Log { task_ids, .. } => {
                let message = LogRequestMessage {
                    task_ids: task_ids.clone(),
                    send_logs: !self.settings.client.read_local_logs,
                };
                Ok(Message::Log(message))
            }
            SubCommand::Follow { task_id, err } => {
                let message = StreamRequestMessage {
                    task_id: *task_id,
                    err: *err,
                };
                Ok(Message::StreamRequest(message))
            }
            SubCommand::Clean => Ok(Message::Clean),
            SubCommand::Reset { children } => Ok(Message::Reset(*children)),
            SubCommand::Shutdown => Ok(Message::DaemonShutdown),
            SubCommand::Parallel {
                parallel_tasks,
                group,
            } => {
                let message = ParallelMessage {
                    parallel_tasks: *parallel_tasks,
                    group: group.clone(),
                };
                Ok(Message::Parallel(message))
            }
            SubCommand::Completions { .. } => bail!("Completions have to be handled earlier"),
            SubCommand::Restart { .. } => bail!("Restarts have to be handled earlier"),
            SubCommand::Edit { .. } => bail!("Edits have to be handled earlier"),
        }
    }
}
