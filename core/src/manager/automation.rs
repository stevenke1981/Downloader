use super::*;

impl Inner {
    pub(super) fn log_automation(&self, message: String) {
        let mut log = self.automation_log.lock().unwrap();
        if log.last() == Some(&message) {
            return;
        }
        log.push(message);
        if log.len() > 100 {
            log.remove(0);
        }
    }

    pub(super) fn run_due_actions(self: &Arc<Self>) {
        let due = {
            let mut settings = self.settings.lock().unwrap();
            let now = chrono::Utc::now().timestamp();
            let mut updated = settings.clone();
            let due: Vec<_> = updated
                .scheduled_actions
                .iter_mut()
                .filter(|action| !action.consumed && action.at <= now)
                .map(|action| {
                    action.consumed = true;
                    (action.id, action.action.clone())
                })
                .collect();
            if due.is_empty() {
                return;
            }
            let saved = serde_json::to_vec_pretty(&updated)
                .map_err(|e| e.to_string())
                .and_then(|json| {
                    write_atomic(&self.dir.join("settings.json"), &json).map_err(|e| e.to_string())
                });
            if let Err(error) = saved {
                self.log_automation(format!(
                    "Actions not executed: consumption could not be saved ({error})"
                ));
                return;
            }
            *settings = updated;
            due
        };
        for (id, action) in due {
            match action {
                Action::StartAll | Action::PauseAll => {
                    let start = action == Action::StartAll;
                    for entry in self.entries.lock().unwrap().iter_mut() {
                        let mut task = entry.task.lock().unwrap();
                        if start && matches!(task.status, Status::Paused | Status::Error) {
                            task.status = Status::Queued;
                            task.error = None;
                            task.verify_retried = false;
                            task.append_log("Queued by scheduled action");
                        } else if !start
                            && matches!(task.status, Status::Queued | Status::Downloading)
                        {
                            task.status = Status::Paused;
                            task.append_log("Paused by scheduled action");
                            if let Some(cancel) = &entry.cancel {
                                cancel.cancel();
                            }
                        }
                    }
                    self.save();
                    self.log_automation(format!("Action {id}: completed"));
                }
                _ => {
                    self.log_automation(format!("Action {id}: started"));
                    let me = self.clone();
                    self.rt.spawn_blocking(move || {
                        let result = crate::automation::run(action);
                        me.log_automation(match result {
                            Ok(()) => format!("Action {id}: completed"),
                            Err(error) => format!("Action {id}: {error}"),
                        });
                    });
                }
            }
        }
    }

}
