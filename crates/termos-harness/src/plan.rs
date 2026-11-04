// SPDX-License-Identifier: GPL-3.0-or-later
//! Plans: what the planning model is asked, and reading what it answers
//! (D-100).
//!
//! The planner answers with a JSON object of typed tasks. Models wrap it
//! in prose or code fences now and then, so the first JSON object in the
//! answer is taken; then it is checked: known kinds, unique ids,
//! dependencies on earlier tasks only, at most `max_tasks` tasks.

use std::path::Path;

use serde::{Deserialize, Serialize};
use termos_core::config::harness::TaskKind;

/// One task of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// `t1`, `t2`, … (unique in the plan).
    pub id: String,
    pub kind: TaskKind,
    pub title: String,
    /// The complete instruction its agent gets.
    pub prompt: String,
    /// Earlier tasks whose results it needs.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// A backend the user picked for it, over the routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

/// A plan: the goal and its tasks, in the order they run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    pub summary: String,
    pub tasks: Vec<Task>,
}

impl Plan {
    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Checks a plan someone edited or wrote: the rules [`parse`] applies.
    pub fn check(&self, max_tasks: usize) -> Result<(), PlanError> {
        if self.tasks.is_empty() {
            return Err(PlanError::Empty);
        }
        if max_tasks > 0 && self.tasks.len() > max_tasks {
            return Err(PlanError::TooMany(self.tasks.len(), max_tasks));
        }
        for (i, t) in self.tasks.iter().enumerate() {
            if t.prompt.trim().is_empty() {
                return Err(PlanError::Invalid(format!("task {} has no prompt", t.id)));
            }
            if self.tasks[..i].iter().any(|o| o.id == t.id) {
                return Err(PlanError::Invalid(format!("two tasks are called {}", t.id)));
            }
            for d in &t.depends_on {
                if !self.tasks[..i].iter().any(|o| &o.id == d) {
                    return Err(PlanError::Invalid(format!(
                        "task {} depends on {d}, which is not an earlier task",
                        t.id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Why an answer is not a plan.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("the planner's answer holds no JSON plan")]
    NoJson,
    #[error("the planner's plan is not valid: {0}")]
    Invalid(String),
    #[error("the plan has no tasks")]
    Empty,
    #[error("the plan has {0} tasks, more than [harness] max-tasks ({1})")]
    TooMany(usize, usize),
}

/// What the planner writes.
#[derive(Debug, Deserialize)]
struct RawPlan {
    #[serde(default)]
    summary: Option<String>,
    tasks: Vec<RawTask>,
}

#[derive(Debug, Deserialize)]
struct RawTask {
    #[serde(default)]
    id: Option<serde_json::Value>,
    kind: String,
    #[serde(default)]
    title: Option<String>,
    prompt: String,
    #[serde(default)]
    depends_on: Vec<serde_json::Value>,
}

fn id_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        serde_json::Value::Number(n) => Some(format!("t{n}")),
        _ => None,
    }
}

/// A title from a prompt's first words.
fn title_of(prompt: &str) -> String {
    let first = prompt.lines().next().unwrap_or("").trim();
    let mut t: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        t.push('…');
    }
    t
}

/// The planner's answer as a plan for `goal`.
pub fn parse(goal: &str, answer: &str, max_tasks: usize) -> Result<Plan, PlanError> {
    let json = first_object(answer).ok_or(PlanError::NoJson)?;
    let raw: RawPlan = serde_json::from_str(json).map_err(|e| PlanError::Invalid(e.to_string()))?;
    let mut tasks: Vec<Task> = Vec::new();
    // Ids as the planner wrote them → the ids kept.
    let mut renamed: Vec<(String, String)> = Vec::new();
    for (i, t) in raw.tasks.into_iter().enumerate() {
        let kind = TaskKind::from_name(&t.kind).ok_or_else(|| {
            PlanError::Invalid(format!(
                "task {} has the kind `{}`; the kinds are {}",
                i + 1,
                t.kind,
                TaskKind::ALL.map(TaskKind::name).join(", ")
            ))
        })?;
        let wanted = t.id.as_ref().and_then(id_text);
        let id = match &wanted {
            Some(w) if !tasks.iter().any(|o| &o.id == w) && w.len() <= 16 => w.clone(),
            _ => format!("t{}", i + 1),
        };
        // Dependencies on earlier tasks only; anything else is dropped
        // (a forward reference would deadlock).
        let mut depends_on = Vec::new();
        for d in t.depends_on.iter().filter_map(id_text) {
            let target = renamed
                .iter()
                .rev()
                .find(|(w, _)| *w == d)
                .map(|(_, k)| k.clone())
                .or_else(|| tasks.iter().find(|o| o.id == d).map(|o| o.id.clone()));
            if let Some(dep) = target
                && dep != id
                && !depends_on.contains(&dep)
            {
                depends_on.push(dep);
            }
        }
        if let Some(w) = wanted {
            renamed.push((w, id.clone()));
        }
        let prompt = t.prompt.trim().to_owned();
        let title = t
            .title
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| title_of(&prompt));
        tasks.push(Task {
            id,
            kind,
            title,
            prompt,
            depends_on,
            backend: None,
        });
    }
    let plan = Plan {
        goal: goal.to_owned(),
        summary: raw.summary.map(|s| s.trim().to_owned()).unwrap_or_default(),
        tasks,
    };
    plan.check(max_tasks)?;
    Ok(plan)
}

/// The first complete JSON object in `text` (inside a code fence or
/// prose), by matching braces outside strings.
pub fn first_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(off) = text[start..].find('{') {
        let begin = start + off;
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (i, &b) in bytes.iter().enumerate().skip(begin) {
            if in_string {
                match (escaped, b) {
                    (true, _) => escaped = false,
                    (false, b'\\') => escaped = true,
                    (false, b'"') => in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let candidate = &text[begin..=i];
                        if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
                            return Some(candidate);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        start = begin + 1;
    }
    None
}

/// What the planning model is asked.
pub fn planner_prompt(goal: &str, project: &Path, max_tasks: usize) -> String {
    let kinds: String = TaskKind::ALL
        .iter()
        .map(|k| format!("- {}: {}\n", k.name(), k.describe()))
        .collect();
    format!(
        "You are planning work for a team of coding agents. Do not change any files; read the \
         project as much as you need to plan well.\n\n\
         Project folder: {project}\n\
         Goal: {goal}\n\n\
         Split the goal into at most {max_tasks} tasks, in the order they should run. Each task has \
         one kind:\n{kinds}\n\
         Each task goes to a separate agent that sees only its own prompt, the project's files and \
         the results of the tasks it depends on. So write each prompt as a complete instruction: \
         what to do, where, and what done looks like. Prefer few, substantial tasks; add none the \
         goal does not need. A small, clear goal can be one task. When code changes, end with a \
         review task that depends on the changes. Use depends_on for every task that needs an \
         earlier task's result.\n\n\
         Answer with only this JSON, and no other text:\n\
         {{\"summary\": \"one sentence\", \"tasks\": [{{\"id\": \"t1\", \"kind\": \"coding\", \
         \"title\": \"a few words\", \"prompt\": \"the full instruction\", \"depends_on\": []}}]}}",
        project = project.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_is_read_from_a_fenced_answer() {
        let answer = r#"Here is the plan:
```json
{"summary": "Add retries", "tasks": [
  {"id": "find", "kind": "search", "title": "Find the client", "prompt": "Find where HTTP calls are made {in src}."},
  {"id": "code", "kind": "coding", "prompt": "Add retries with backoff.\nKeep it small.", "depends_on": ["find"]},
  {"id": "check", "kind": "Review", "prompt": "Review the change.", "depends_on": ["code", "later", "check"]}
]}
```
Done."#;
        let plan = parse("retries", answer, 8).expect("a plan");
        assert_eq!(plan.summary, "Add retries");
        let ids: Vec<&str> = plan.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["find", "code", "check"]);
        assert_eq!(plan.tasks[1].title, "Add retries with backoff.");
        assert_eq!(plan.tasks[1].depends_on, ["find"]);
        // Unknown and self references are dropped.
        assert_eq!(plan.tasks[2].depends_on, ["code"]);
        assert_eq!(plan.tasks[2].kind, TaskKind::Review);
        assert_eq!(
            plan.tasks[0].prompt,
            "Find where HTTP calls are made {in src}."
        );
    }

    #[test]
    fn bad_plans_say_why() {
        assert_eq!(parse("g", "no json here", 8), Err(PlanError::NoJson));
        assert!(matches!(
            parse("g", r#"{"tasks": [{"kind": "dance", "prompt": "x"}]}"#, 8),
            Err(PlanError::Invalid(e)) if e.contains("dance")
        ));
        assert_eq!(parse("g", r#"{"tasks": []}"#, 8), Err(PlanError::Empty));
        let many = format!(
            "{{\"tasks\": [{}]}}",
            [r#"{"kind": "docs", "prompt": "x"}"#; 3].join(",")
        );
        assert_eq!(parse("g", &many, 2), Err(PlanError::TooMany(3, 2)));
        // Duplicate or missing ids are numbered.
        let plan = parse(
            "g",
            r#"{"tasks": [{"id": "a", "kind": "docs", "prompt": "x"}, {"id": "a", "kind": "docs", "prompt": "y", "depends_on": ["a"]}, {"kind": "tests", "prompt": "z", "depends_on": [2]}]}"#,
            8,
        )
        .expect("plan");
        let ids: Vec<&str> = plan.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a", "t2", "t3"]);
        assert_eq!(plan.tasks[1].depends_on, ["a"]);
        assert_eq!(plan.tasks[2].depends_on, ["t2"]);
    }

    #[test]
    fn edited_plans_are_checked() {
        let mut plan = parse(
            "g",
            r#"{"tasks": [{"kind": "coding", "prompt": "x"}, {"kind": "review", "prompt": "y", "depends_on": ["t1"]}]}"#,
            8,
        )
        .expect("plan");
        assert!(plan.check(8).is_ok());
        plan.tasks.swap(0, 1);
        assert!(plan.check(8).is_err(), "a dependency now comes later");
    }

    #[test]
    fn the_first_object_skips_braces_in_strings_and_prose() {
        assert_eq!(
            first_object("a {b} {\"x\": \"}\"} c"),
            Some("{\"x\": \"}\"}")
        );
        assert_eq!(
            first_object("{\"a\": {\"b\": 1}}"),
            Some("{\"a\": {\"b\": 1}}")
        );
        assert_eq!(first_object("{ unterminated"), None);
        assert_eq!(first_object("}{"), None);
    }

    #[test]
    fn the_prompt_names_every_kind() {
        let p = planner_prompt("make it fast", Path::new("/work/app"), 5);
        for k in TaskKind::ALL {
            assert!(p.contains(&format!("- {}:", k.name())), "{}", k.name());
        }
        assert!(p.contains("at most 5 tasks") && p.contains("/work/app"));
    }
}
