use super::*;

impl Inner {
    /// Timer loop only: update speeds from the bytes moved over the real elapsed `dt`, then dispatch.
    pub(super) fn tick(self: &Arc<Self>, dt: f64) {
        self.run_due_actions();
        {
            let mut es = self.entries.lock().unwrap();
            for e in es.iter_mut() {
                let b = e.task.lock().unwrap().downloaded();
                let inst = b.saturating_sub(e.last_bytes) as f64 / dt;
                e.speed = if e.speed == 0.0 {
                    inst
                } else {
                    e.speed * 0.6 + inst * 0.4
                };
                e.last_bytes = b;
            }
        }
        self.dispatch();
    }

    /// Reap finished jobs and start queued ones. Never touches speeds (no real dt here).
    pub(super) fn dispatch(self: &Arc<Self>) {
        let mut es = self.entries.lock().unwrap();
        let settings = self.settings.lock().unwrap().clone();
        let allowed = settings.schedule.allows_now();
        for e in es.iter_mut() {
            if e.handle.as_ref().is_some_and(|h| h.is_finished()) {
                e.handle = None;
                e.cancel = None;
            }
        }
        if !allowed {
            // Outside the schedule window: put running downloads back in the queue.
            for e in es.iter_mut() {
                let mut t = e.task.lock().unwrap();
                if t.status == Status::Downloading {
                    t.status = Status::Queued;
                    if let Some(c) = &e.cancel {
                        c.cancel();
                    }
                }
            }
            return;
        }
        let running = es.iter().filter(|e| e.running()).count();
        let slots = (settings.max_concurrent.max(1) as usize).saturating_sub(running);
        if slots == 0 {
            return;
        }
        let mut queued: Vec<(i32, u64, usize)> = es
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.running())
            .filter_map(|(i, e)| {
                let t = e.task.lock().unwrap();
                (t.status == Status::Queued).then_some((-t.priority, t.id, i))
            })
            .collect();
        queued.sort();
        let client = self.client.lock().unwrap().clone();
        let mut per_site: HashMap<String, u32> = HashMap::new();
        for entry in es.iter().filter(|entry| entry.running()) {
            if let Some(origin) = crate::site::url_origin(&entry.task.lock().unwrap().url) {
                *per_site.entry(origin).or_default() += 1;
            }
        }
        let mut started = 0;
        for (_, _, i) in queued {
            if started == slots {
                break;
            }
            let origin = crate::site::url_origin(&es[i].task.lock().unwrap().url);
            if let Some(origin) = &origin
                && settings
                    .site_rules
                    .iter()
                    .find(|rule| &rule.host == origin)
                    .is_some_and(|rule| {
                        per_site.get(origin).copied().unwrap_or(0) >= rule.max_concurrent
                    })
            {
                continue;
            }
            if let Some(origin) = origin {
                *per_site.entry(origin).or_default() += 1;
            }
            started += 1;
            let e = &mut es[i];
            let cancel = CancellationToken::new();
            let (downloaded, speed_limit) = {
                let mut t = e.task.lock().unwrap();
                t.status = Status::Downloading;
                t.append_log("Download started");
                (t.downloaded(), t.speed_limit)
            };
            e.limiter.set_rate(speed_limit);
            // Measure this run's speed from where it resumes, not from zero.
            e.last_bytes = downloaded;
            e.speed = 0.0;
            let ctx = Ctx {
                client: client.clone(),
                protocol_client: self.protocol_client.lock().unwrap().clone(),
                global: self.global.clone(),
                local: e.limiter.clone(),
                retries: settings.retries,
                cancel: cancel.clone(),
                verify: settings.auto_verify,
            };
            let task = e.task.clone();
            let me = self.clone();
            e.cancel = Some(cancel);
            e.handle = Some(self.rt.spawn(async move {
                let r = engine::run(task.clone(), ctx).await;
                {
                    let mut t = task.lock().unwrap();
                    match r {
                        Ok(Outcome::Completed) => {
                            t.status = Status::Completed;
                            t.error = None;
                            t.append_log("Download completed");
                        }
                        Ok(Outcome::Cancelled) => {
                            if t.status == Status::Downloading {
                                t.status = Status::Paused;
                                t.append_log("Download paused");
                            }
                        }
                        Err(err) => {
                            t.status = Status::Error;
                            t.error = Some(err);
                            t.append_log("Download failed; see error status");
                        }
                    }
                }
                me.save();
            }));
        }
    }
}
