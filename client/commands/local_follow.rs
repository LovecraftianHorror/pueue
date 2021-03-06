use ::anyhow::{bail, Result};
use ::async_std::net::TcpStream;

use crate::commands::get_state;
use crate::output::follow_task_logs;

pub async fn local_follow(
    socket: &mut TcpStream,
    pueue_directory: String,
    task_id: &Option<usize>,
    err: bool,
) -> Result<()> {
    // The user can specify the id of the task they want to follow
    // If the id isn't specified and there's only a single running task, this task will be used.
    // However, if there are multiple running tasks, the user will have to specify an id.
    let task_id = match task_id {
        Some(task_id) => *task_id,
        None => {
            let state = get_state(&mut socket.clone()).await?;
            let running_ids: Vec<_> = state
                .tasks
                .iter()
                .filter_map(|(&id, t)| if t.is_running() { Some(id) } else { None })
                .collect();

            match running_ids.len() {
                0 => {
                    bail!("There are no running tasks.");
                }
                1 => running_ids[0],
                _ => {
                    let running_ids = running_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    bail!(
                        "Multiple tasks are running, please select one of the following: {}",
                        running_ids
                    );
                }
            }
        }
    };

    follow_task_logs(pueue_directory, task_id, err);

    Ok(())
}
