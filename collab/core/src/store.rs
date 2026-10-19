use crate::model::*;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

pub struct Store {
    db: Mutex<Connection>,
    pub root: PathBuf,
    _lock: std::fs::File,
}
fn append(
    c: &Connection,
    session: &str,
    agent: Option<&str>,
    task: Option<&str>,
    kind: &str,
    data: &Value,
) -> Result<i64> {
    c.execute(
        "INSERT INTO events(session,agent,task,kind,data,created) VALUES(?,?,?,?,?,?)",
        params![session, agent, task, kind, data.to_string(), now()],
    )?;
    Ok(c.last_insert_rowid())
}
fn save_task(c: &Connection, t: &Task) -> Result<()> {
    c.execute(
        "UPDATE tasks SET data=?,status=?,integration=? WHERE id=?",
        params![
            serde_json::to_string(t)?,
            t.status,
            t.integration_status,
            t.id
        ],
    )?;
    Ok(())
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("service.lock"))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            ensure!(
                unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
                "collaboration state is already open by another host"
            );
        }
        let c = Connection::open(root.join("collab.sqlite"))?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        let schema: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(schema <= 1, "state database is newer than this application");
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY,session TEXT NOT NULL REFERENCES sessions(id),data TEXT NOT NULL,cursor INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,session TEXT NOT NULL REFERENCES sessions(id),agent TEXT NOT NULL REFERENCES agents(id),status TEXT NOT NULL,integration TEXT NOT NULL,data TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS tasks_state ON tasks(status,agent);
            CREATE TABLE IF NOT EXISTS events(revision INTEGER PRIMARY KEY AUTOINCREMENT,session TEXT NOT NULL REFERENCES sessions(id),agent TEXT,task TEXT,kind TEXT NOT NULL,data TEXT NOT NULL,created INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS events_session ON events(session,revision);
            CREATE TABLE IF NOT EXISTS approvals(id TEXT PRIMARY KEY,task TEXT NOT NULL REFERENCES tasks(id),data TEXT NOT NULL);
            PRAGMA user_version=1;")?;
        Ok(Self {
            db: Mutex::new(c),
            root: root.to_path_buf(),
            _lock: lock,
        })
    }
    pub fn add_session(&self, s: &Session) -> Result<()> {
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO sessions VALUES(?,?)",
            params![s.id, serde_json::to_string(s)?],
        )?;
        append(
            &tx,
            &s.id,
            None,
            None,
            "session_created",
            &json!({"baseCommit":s.base_commit}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn session(&self, id: &str) -> Result<Session> {
        let c = self.db.lock().unwrap();
        let s: String = c.query_row("SELECT data FROM sessions WHERE id=?", [id], |r| r.get(0))?;
        Ok(serde_json::from_str(&s)?)
    }
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let c = self.db.lock().unwrap();
        let mut q = c.prepare("SELECT data FROM sessions ORDER BY rowid")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|s| Ok(serde_json::from_str(&s?)?)).collect()
    }
    pub fn configure(&self, a: &Agent) -> Result<()> {
        a.validate()?;
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let existing: Option<(String, String)> = tx
            .query_row("SELECT session,data FROM agents WHERE id=?", [&a.id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((session, data)) = &existing {
            ensure!(
                session == &a.session_id,
                "cannot move agent between sessions"
            );
            let old: Agent = serde_json::from_str(data)?;
            ensure!(old.role == a.role, "cannot change agent role");
        }
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM tasks WHERE agent=? AND status='running'",
            [&a.id],
            |r| r.get(0),
        )?;
        ensure!(active == 0, "stop current task before reconfiguring agent");
        let mut q = tx.prepare("SELECT data FROM agents WHERE session=? AND id<>?")?;
        let others: Vec<Agent> = q
            .query_map(params![a.session_id, a.id], |r| r.get::<_, String>(0))?
            .map(|x| Ok(serde_json::from_str(&x?)?))
            .collect::<Result<_>>()?;
        drop(q);
        ensure!(others.len() < 4, "a session supports at most four agents");
        ensure!(
            a.role != Role::Coordinator || !others.iter().any(|x| x.role == Role::Coordinator),
            "coordinator already exists"
        );
        ensure!(
            a.role != Role::Worker || others.iter().filter(|x| x.role == Role::Worker).count() < 3,
            "at most three workers"
        );
        tx.execute("INSERT INTO agents(id,session,data,cursor) VALUES(?,?,?,0) ON CONFLICT(id) DO UPDATE SET data=excluded.data,cursor=0",params![a.id,a.session_id,serde_json::to_string(a)?])?;
        append(
            &tx,
            &a.session_id,
            Some(&a.id),
            None,
            "agent_configured",
            &json!({"model":a.model,"protocol":a.protocol,"role":a.role}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn agent(&self, id: &str) -> Result<Agent> {
        let c = self.db.lock().unwrap();
        let data: String = c.query_row("SELECT data FROM agents WHERE id=?", [id], |r| r.get(0))?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn agents(&self, session: &str) -> Result<Vec<Agent>> {
        let c = self.db.lock().unwrap();
        let mut q = c.prepare("SELECT data FROM agents WHERE session=? ORDER BY rowid")?;
        q.query_map([session], |r| r.get::<_, String>(0))?
            .map(|x| Ok(serde_json::from_str(&x?)?))
            .collect()
    }
    pub fn create_task(&self, r: TaskRequest) -> Result<Task> {
        ensure!(
            !r.goal.trim().is_empty() && r.goal.len() <= 64_000 && r.acceptance.len() <= 16_000,
            "invalid task text"
        );
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let session: String = tx.query_row(
            "SELECT session FROM agents WHERE id=?",
            [&r.agent_id],
            |r| r.get(0),
        )?;
        ensure!(session == r.session_id, "agent belongs to another session");
        for d in &r.dependencies {
            let s: String =
                tx.query_row("SELECT session FROM tasks WHERE id=?", [d], |r| r.get(0))?;
            ensure!(s == r.session_id, "dependency belongs to another session");
        }
        // Dependencies only reference previously created tasks, so cycles cannot be introduced.
        let t = Task {
            id: id(),
            request: r,
            status: "queued".into(),
            context_version: 0,
            worktree: None,
            base_commit: None,
            commit: None,
            result: None,
            integration_status: "pending".into(),
        };
        tx.execute(
            "INSERT INTO tasks VALUES(?,?,?,?,?,?)",
            params![
                t.id,
                t.request.session_id,
                t.request.agent_id,
                t.status,
                t.integration_status,
                serde_json::to_string(&t)?
            ],
        )?;
        append(
            &tx,
            &t.request.session_id,
            Some(&t.request.agent_id),
            Some(&t.id),
            "task_queued",
            &json!({"goal":t.request.goal,"acceptance":t.request.acceptance,"dependencies":t.request.dependencies}),
        )?;
        tx.commit()?;
        Ok(t)
    }
    pub fn task(&self, id: &str) -> Result<Task> {
        let c = self.db.lock().unwrap();
        let s: String = c.query_row("SELECT data FROM tasks WHERE id=?", [id], |r| r.get(0))?;
        Ok(serde_json::from_str(&s)?)
    }
    pub fn tasks(&self, session: &str) -> Result<Vec<Task>> {
        let c = self.db.lock().unwrap();
        let mut q = c.prepare("SELECT data FROM tasks WHERE session=? ORDER BY rowid")?;
        q.query_map([session], |r| r.get::<_, String>(0))?
            .map(|x| Ok(serde_json::from_str(&x?)?))
            .collect()
    }
    pub fn claim(&self) -> Result<Option<Task>> {
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM tasks WHERE status='running'",
            [],
            |r| r.get(0),
        )?;
        if active >= 4 {
            return Ok(None);
        }
        let mut q=tx.prepare("SELECT data FROM tasks WHERE status='queued' AND agent NOT IN (SELECT agent FROM tasks WHERE status='running') ORDER BY rowid")?;
        let candidates: Vec<Task> = q
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|x| Ok(serde_json::from_str(&x?)?))
            .collect::<Result<_>>()?;
        drop(q);
        for mut t in candidates {
            let mut ready = true;
            let mut blocked = false;
            for d in &t.request.dependencies {
                let (status, integration): (String, String) = tx.query_row(
                    "SELECT status,integration FROM tasks WHERE id=?",
                    [d],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if matches!(
                    status.as_str(),
                    "failed" | "cancelled" | "interrupted" | "blocked"
                ) {
                    blocked = true;
                }
                let data: String =
                    tx.query_row("SELECT data FROM tasks WHERE id=?", [d], |r| r.get(0))?;
                let dependency: Task = serde_json::from_str(&data)?;
                ready &= status == "completed"
                    && (!dependency.request.coding || integration == "integrated");
            }
            if blocked {
                t.status = "blocked".into();
                save_task(&tx, &t)?;
                append(
                    &tx,
                    &t.request.session_id,
                    Some(&t.request.agent_id),
                    Some(&t.id),
                    "task_blocked",
                    &json!({"reason":"dependency did not complete; create a replacement task explicitly"}),
                )?;
                continue;
            }
            if ready {
                t.status = "running".into();
                save_task(&tx, &t)?;
                append(
                    &tx,
                    &t.request.session_id,
                    Some(&t.request.agent_id),
                    Some(&t.id),
                    "task_started",
                    &json!({}),
                )?;
                tx.commit()?;
                return Ok(Some(t));
            }
        }
        tx.commit()?;
        Ok(None)
    }
    pub fn workspace(&self, id: &str, path: &Path, base: &str) -> Result<()> {
        self.update_running(id, |t| {
            t.worktree = Some(path.to_path_buf());
            t.base_commit = Some(base.into());
        })
    }
    fn update_running(&self, id: &str, f: impl FnOnce(&mut Task)) -> Result<()> {
        let c = self.db.lock().unwrap();
        let s: String = c.query_row("SELECT data FROM tasks WHERE id=?", [id], |r| r.get(0))?;
        let mut t: Task = serde_json::from_str(&s)?;
        ensure!(t.status == "running", "task is no longer running");
        f(&mut t);
        save_task(&c, &t)
    }
    pub fn finish(
        &self,
        id: &str,
        status: &str,
        result: &str,
        commit: Option<&str>,
    ) -> Result<bool> {
        ensure!(
            matches!(status, "completed" | "failed" | "cancelled"),
            "invalid terminal state"
        );
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let s: String = tx.query_row("SELECT data FROM tasks WHERE id=?", [id], |r| r.get(0))?;
        let mut t: Task = serde_json::from_str(&s)?;
        if !matches!(t.status.as_str(), "running" | "queued") {
            return Ok(false);
        }
        t.status = status.into();
        t.result = Some(clip(result, 16_000));
        t.commit = commit.map(str::to_owned);
        save_task(&tx, &t)?;
        append(
            &tx,
            &t.request.session_id,
            Some(&t.request.agent_id),
            Some(id),
            "task_finished",
            &json!({"status":status,"summary":t.result,"commit":t.commit}),
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn integration(&self, id: &str, status: &str, evidence: &Value) -> Result<()> {
        ensure!(
            matches!(
                status,
                "checking" | "integrated" | "conflict" | "failed" | "interrupted"
            ),
            "invalid integration state"
        );
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let s: String = tx.query_row("SELECT data FROM tasks WHERE id=?", [id], |r| r.get(0))?;
        let mut t: Task = serde_json::from_str(&s)?;
        ensure!(
            t.status == "completed" && t.request.coding,
            "only completed coding tasks can be integrated"
        );
        ensure!(t.integration_status != "integrated", "already integrated");
        t.integration_status = status.into();
        save_task(&tx, &t)?;
        append(
            &tx,
            &t.request.session_id,
            None,
            Some(id),
            "integration",
            &json!({"status":status,"evidence":evidence}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn recover(&self) -> Result<usize> {
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let mut q =
            tx.prepare("SELECT data FROM tasks WHERE status='running' OR integration='checking'")?;
        let tasks: Vec<Task> = q
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|x| Ok(serde_json::from_str(&x?)?))
            .collect::<Result<_>>()?;
        drop(q);
        for mut t in tasks.iter().cloned() {
            if t.status == "running" {
                t.status = "interrupted".into();
            }
            if t.integration_status == "checking" {
                t.integration_status = "interrupted".into();
            }
            save_task(&tx, &t)?;
            append(
                &tx,
                &t.request.session_id,
                Some(&t.request.agent_id),
                Some(&t.id),
                "recovery_required",
                &json!({"reason":"process stopped; inspect files and effects before creating replacement work"}),
            )?;
        }
        tx.execute("DELETE FROM approvals", [])?;
        tx.commit()?;
        Ok(tasks.len())
    }
    pub fn publish(
        &self,
        session: &str,
        agent: Option<&str>,
        task: Option<&str>,
        kind: &str,
        data: &Value,
    ) -> Result<i64> {
        ensure!(
            matches!(
                kind,
                "requirement"
                    | "user_override"
                    | "decision"
                    | "suggestion"
                    | "result"
                    | "public_message"
                    | "usage"
                    | "artifact"
                    | "approval_requested"
            ),
            "invalid shared event kind"
        );
        if let Some(a) = agent {
            let a = self.agent(a)?;
            ensure!(a.session_id == session, "wrong session");
            ensure!(
                !matches!(kind, "requirement" | "user_override"),
                "user-only event"
            );
            if kind == "decision" {
                ensure!(
                    a.role == Role::Coordinator,
                    "only coordinator can publish decisions"
                );
            }
        }
        if let Some(t) = task {
            ensure!(
                self.task(t)?.request.session_id == session,
                "wrong task session"
            );
        }
        ensure!(
            data.to_string().len() <= 64_000,
            "event too large; save an attachment"
        );
        let c = self.db.lock().unwrap();
        append(&c, session, agent, task, kind, data)
    }
    pub fn has_event(&self, task: &str, kind: &str, data: Option<&Value>) -> Result<bool> {
        let c = self.db.lock().unwrap();
        let count: i64 = if let Some(data) = data {
            c.query_row(
                "SELECT count(*) FROM events WHERE task=? AND kind=? AND data=?",
                params![task, kind, data.to_string()],
                |r| r.get(0),
            )?
        } else {
            c.query_row(
                "SELECT count(*) FROM events WHERE task=? AND kind=?",
                params![task, kind],
                |r| r.get(0),
            )?
        };
        Ok(count > 0)
    }
    pub fn events(&self, session: &str, after: i64, limit: usize) -> Result<Vec<Event>> {
        let c = self.db.lock().unwrap();
        let mut q=c.prepare("SELECT revision,agent,task,kind,data,created FROM events WHERE session=? AND revision>? ORDER BY revision LIMIT ?")?;
        let rows = q.query_map(params![session, after, limit.min(1000) as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        rows.map(|r| {
            let (v, a, t, k, d, at) = r?;
            Ok(Event {
                revision: v,
                session_id: session.into(),
                agent_id: a,
                task_id: t,
                kind: k,
                data: serde_json::from_str(&d)?,
                created_at: at,
            })
        })
        .collect()
    }
    pub fn context(&self, agent: &Agent, task: &Task) -> Result<(i64, String)> {
        let c = self.db.lock().unwrap();
        let version: i64 = c.query_row(
            "SELECT coalesce(max(revision),0) FROM events WHERE session=?",
            [&agent.session_id],
            |r| r.get(0),
        )?;
        let cursor: i64 =
            c.query_row("SELECT cursor FROM agents WHERE id=?", [&agent.id], |r| {
                r.get(0)
            })?;
        // Materialized authoritative records remain present on EVERY request: injected context
        // is not part of the app-server's own conversation history.
        let checkpoint:Option<(i64,String)>=c.query_row("SELECT revision,data FROM events WHERE session=? AND kind='checkpoint' ORDER BY revision DESC LIMIT 1",[&agent.session_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let through = checkpoint.as_ref().map(|x| x.0).unwrap_or(0);
        let mut q=c.prepare("SELECT kind,data,revision,agent FROM events WHERE session=? AND (kind IN ('requirement','user_override') OR (kind='decision' AND revision>?)) ORDER BY revision")?;
        let formal:Vec<Value>=q.query_map(params![agent.session_id,through],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<String>>(3)?)))?.map(|r|{let(k,d,v,a)=r?;Ok(json!({"kind":k,"data":serde_json::from_str::<Value>(&d)?,"revision":v,"agentId":a}))}).collect::<Result<_>>()?;
        let mut q=c.prepare("SELECT kind,data,revision,agent,task FROM events WHERE session=? AND revision>? AND kind NOT IN ('public_message','usage') ORDER BY revision DESC LIMIT 30")?;
        let recent:Vec<Value>=q.query_map(params![agent.session_id,through],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?)))?.map(|r|{let(k,d,v,a,t)=r?;Ok(json!({"kind":k,"data":serde_json::from_str::<Value>(&d)?,"revision":v,"agentId":a,"taskId":t}))}).collect::<Result<_>>()?;
        let mut roster_query =
            c.prepare("SELECT data FROM agents WHERE session=? ORDER BY rowid")?;
        let roster: Vec<Value> = roster_query
            .query_map([&agent.session_id], |r| r.get::<_, String>(0))?
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<_>>()?;
        let text=json!({"agents":roster,"revision":version,"previouslyConsumed":cursor,"task":{"id":task.id,"goal":task.request.goal,"acceptance":task.request.acceptance},"checkpoint":checkpoint.map(|(_,d)|serde_json::from_str::<Value>(&d)).transpose()?,"formalRecords":formal,"recentRecordsNewestFirst":recent}).to_string();
        ensure!(
            text.len() <= agent.context_bytes,
            "shared context exceeds configured budget; coordinator must compact formal records before continuing"
        );
        Ok((version, text))
    }
    pub fn acknowledge(&self, agent: &str, task: &str, version: i64) -> Result<()> {
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE agents SET cursor=max(cursor,?) WHERE id=?",
            params![version, agent],
        )?;
        let s: String = tx.query_row("SELECT data FROM tasks WHERE id=?", [task], |r| r.get(0))?;
        let mut t: Task = serde_json::from_str(&s)?;
        t.context_version = t.context_version.max(version);
        save_task(&tx, &t)?;
        tx.commit()?;
        Ok(())
    }
    pub fn attachment(&self, task: &Task, label: &str, content: &str) -> Result<PathBuf> {
        let dir = self.root.join("attachments").join(&task.id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.txt", id()));
        std::fs::write(&path, content)?;
        self.publish(
            &task.request.session_id,
            Some(&task.request.agent_id),
            Some(&task.id),
            "artifact",
            &json!({"label":label,"path":path,"bytes":content.len(),"preview":clip(content,1000)}),
        )?;
        Ok(path)
    }
    pub fn add_approval(&self, a: &Approval) -> Result<()> {
        let c = self.db.lock().unwrap();
        c.execute(
            "INSERT INTO approvals VALUES(?,?,?)",
            params![a.id, a.task_id, serde_json::to_string(a)?],
        )?;
        Ok(())
    }
    pub fn approvals(&self) -> Result<Vec<Approval>> {
        let c = self.db.lock().unwrap();
        let mut q = c.prepare("SELECT data FROM approvals ORDER BY rowid")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect()
    }
    pub fn decide(&self, id: &str, accept: bool) -> Result<()> {
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let s: String =
            tx.query_row("SELECT data FROM approvals WHERE id=?", [id], |r| r.get(0))?;
        let mut a: Approval = serde_json::from_str(&s)?;
        ensure!(a.decision.is_none(), "approval already resolved");
        let status: String =
            tx.query_row("SELECT status FROM tasks WHERE id=?", [&a.task_id], |r| {
                r.get(0)
            })?;
        ensure!(status == "running", "task no longer active");
        a.decision = Some(accept);
        tx.execute(
            "UPDATE approvals SET data=? WHERE id=?",
            params![serde_json::to_string(&a)?, id],
        )?;
        append(
            &tx,
            &a.session_id,
            Some(&a.agent_id),
            Some(&a.task_id),
            "approval_resolved",
            &json!({"approvalId":id,"accepted":accept,"source":"user"}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn approval_decision(&self, id: &str) -> Result<Option<bool>> {
        self.approvals()?
            .into_iter()
            .find(|a| a.id == id)
            .map(|a| a.decision)
            .ok_or_else(|| anyhow::anyhow!("approval unavailable"))
    }
    pub fn delete_approval(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("DELETE FROM approvals WHERE id=?", [id])?;
        Ok(())
    }
    pub fn compact(
        &self,
        session: &str,
        actor: Option<&str>,
        summary: &str,
        expected: i64,
    ) -> Result<i64> {
        ensure!(
            !summary.is_empty() && summary.len() <= 8192,
            "summary must contain 1..8192 bytes"
        );
        if let Some(actor) = actor {
            let a = self.agent(actor)?;
            ensure!(
                a.session_id == session && a.role == Role::Coordinator,
                "coordinator required"
            );
        }
        let mut c = self.db.lock().unwrap();
        let tx = c.transaction()?;
        let actual: i64 = tx.query_row(
            "SELECT coalesce(max(revision),0) FROM events WHERE session=?",
            [session],
            |r| r.get(0),
        )?;
        ensure!(
            actual == expected,
            "context changed; refresh before compacting"
        );
        // Do not remove user constraints or the audit log. A checkpoint replaces only old
        // model decisions in the materialized view (selection uses checkpoint in context).
        let rev = append(
            &tx,
            session,
            actor,
            None,
            "checkpoint",
            &json!({"summary":summary,"throughRevision":expected}),
        )?;
        tx.commit()?;
        Ok(rev)
    }
}
