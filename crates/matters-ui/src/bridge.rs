use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use matters_core::{
    Area, ChecklistItem, Database, Project, QueryEngine, Task,
};
use uuid::Uuid;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, current_view)]
        #[qproperty(QString, current_title)]
        #[qproperty(QString, current_project_id)]
        #[qproperty(f64, current_project_progress)]
        #[qproperty(i32, inbox_count)]
        #[qproperty(i32, today_count)]
        #[qproperty(i32, upcoming_count)]
        #[qproperty(i32, anytime_count)]
        #[qproperty(i32, someday_count)]
        #[qproperty(i32, logbook_count)]
        #[qproperty(QString, tasks_json)]
        #[qproperty(QString, projects_json)]
        #[qproperty(QString, areas_json)]
        #[qproperty(QString, search_query)]
        #[qproperty(QString, search_results_json)]
        type AppController = super::AppControllerRust;

        #[qinvokable]
        fn select_view(self: Pin<&mut AppController>, view: &QString);

        #[qinvokable]
        fn select_project(self: Pin<&mut AppController>, project_id: &QString);

        #[qinvokable]
        fn add_task(self: Pin<&mut AppController>, title: &QString, when: &QString);

        #[qinvokable]
        fn add_task_to_project(self: Pin<&mut AppController>, title: &QString, project_id: &QString);

        #[qinvokable]
        fn toggle_task(self: Pin<&mut AppController>, task_id: &QString);

        #[qinvokable]
        fn delete_task(self: Pin<&mut AppController>, task_id: &QString);

        #[qinvokable]
        fn schedule_task(self: Pin<&mut AppController>, task_id: &QString, when: &QString);

        #[qinvokable]
        fn update_task_notes(self: Pin<&mut AppController>, task_id: &QString, notes: &QString);

        #[qinvokable]
        fn add_checklist_item(self: Pin<&mut AppController>, task_id: &QString, title: &QString);

        #[qinvokable]
        fn toggle_checklist_item(self: Pin<&mut AppController>, item_id: &QString);

        #[qinvokable]
        fn get_checklist_json(self: &AppController, task_id: &QString) -> QString;

        #[qinvokable]
        fn create_project(self: Pin<&mut AppController>, title: &QString, area_id: &QString);

        #[qinvokable]
        fn create_area(self: Pin<&mut AppController>, title: &QString);

        #[qinvokable]
        fn search(self: Pin<&mut AppController>, query: &QString);

        #[qinvokable]
        fn refresh(self: Pin<&mut AppController>);
    }

    impl cxx_qt::Initialize for AppController {}
}

pub struct AppControllerRust {
    pub current_view: QString,
    pub current_title: QString,
    pub current_project_id: QString,
    pub current_project_progress: f64,
    pub inbox_count: i32,
    pub today_count: i32,
    pub upcoming_count: i32,
    pub anytime_count: i32,
    pub someday_count: i32,
    pub logbook_count: i32,
    pub tasks_json: QString,
    pub projects_json: QString,
    pub areas_json: QString,
    pub search_query: QString,
    pub search_results_json: QString,

    pub db: Database,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        Self {
            current_view: QString::from("today"),
            current_title: QString::from("Today"),
            current_project_id: QString::default(),
            current_project_progress: 0.0,
            inbox_count: 0,
            today_count: 0,
            upcoming_count: 0,
            anytime_count: 0,
            someday_count: 0,
            logbook_count: 0,
            tasks_json: QString::from("[]"),
            projects_json: QString::from("[]"),
            areas_json: QString::from("[]"),
            search_query: QString::default(),
            search_results_json: QString::from("[]"),
            db: Database::default(),
        }
    }
}

impl cxx_qt::Initialize for qobject::AppController {
    fn initialize(self: Pin<&mut Self>) {
        self.refresh();
    }
}

impl qobject::AppController {
    pub fn select_view(mut self: Pin<&mut Self>, view: &QString) {
        let view_str = String::from(view);
        let title = match view_str.as_str() {
            "inbox" => "Inbox",
            "today" => "Today",
            "upcoming" => "Upcoming",
            "anytime" => "Anytime",
            "someday" => "Someday",
            "logbook" => "Logbook",
            _ => "Tasks",
        };

        self.as_mut().set_current_view(QString::from(view_str.as_str()));
        self.as_mut().set_current_title(QString::from(title));
        self.as_mut().set_current_project_id(QString::default());
        self.refresh();
    }

    pub fn select_project(mut self: Pin<&mut Self>, project_id: &QString) {
        let proj_id_str = String::from(project_id);
        let title = {
            let rust = self.rust();
            if let Ok(Some(p)) = rust.db.get_project(&proj_id_str) {
                p.title
            } else {
                "Project".to_string()
            }
        };

        self.as_mut().set_current_view(QString::from("project"));
        self.as_mut().set_current_title(QString::from(title.as_str()));
        self.as_mut().set_current_project_id(QString::from(proj_id_str.as_str()));
        self.refresh();
    }

    pub fn add_task(self: Pin<&mut Self>, title: &QString, when: &QString) {
        let title_str = String::from(title).trim().to_string();
        if title_str.is_empty() {
            return;
        }

        let when_str = String::from(when);
        let today = chrono::Local::now().date_naive();
        let mut task = Task::new(Uuid::new_v4().to_string(), title_str, 0.0);

        match when_str.as_str() {
            "today" => {
                task.start_date = Some(today);
            }
            "evening" => {
                task.start_date = Some(today);
                task.time_of_day = Some("evening".to_string());
            }
            "someday" => {
                task.is_someday = true;
            }
            date_str if !date_str.is_empty() => {
                if let Ok(parsed) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    task.start_date = Some(parsed);
                }
            }
            _ => {
                // If in today view, default to today; otherwise unassigned (inbox)
                if self.rust().current_view == QString::from("today") {
                    task.start_date = Some(today);
                }
            }
        }

        let _ = self.rust().db.create_task(&task);
        self.refresh();
    }

    pub fn add_task_to_project(self: Pin<&mut Self>, title: &QString, project_id: &QString) {
        let title_str = String::from(title).trim().to_string();
        let proj_id_str = String::from(project_id);
        if title_str.is_empty() || proj_id_str.is_empty() {
            return;
        }

        let mut task = Task::new(Uuid::new_v4().to_string(), title_str, 0.0);
        task.project_id = Some(proj_id_str);

        let _ = self.rust().db.create_task(&task);
        self.refresh();
    }

    pub fn toggle_task(self: Pin<&mut Self>, task_id: &QString) {
        let id_str = String::from(task_id);
        let rust = self.rust();
        if let Ok(Some(task)) = rust.db.get_task(&id_str) {
            if task.status == matters_core::Status::Completed {
                let _ = rust.db.uncomplete_task(&id_str);
            } else {
                let _ = rust.db.complete_task(&id_str);
            }
        }
        self.refresh();
    }

    pub fn delete_task(self: Pin<&mut Self>, task_id: &QString) {
        let id_str = String::from(task_id);
        let _ = self.rust().db.delete_task(&id_str);
        self.refresh();
    }

    pub fn schedule_task(self: Pin<&mut Self>, task_id: &QString, when: &QString) {
        let id_str = String::from(task_id);
        let when_str = String::from(when);
        let today = chrono::Local::now().date_naive();

        if let Ok(Some(mut task)) = self.rust().db.get_task(&id_str) {
            match when_str.as_str() {
                "inbox" => {
                    task.start_date = None;
                    task.time_of_day = None;
                    task.is_someday = false;
                    task.project_id = None;
                    task.area_id = None;
                }
                "today" => {
                    task.start_date = Some(today);
                    task.time_of_day = None;
                    task.is_someday = false;
                }
                "evening" => {
                    task.start_date = Some(today);
                    task.time_of_day = Some("evening".to_string());
                    task.is_someday = false;
                }
                "someday" => {
                    task.start_date = None;
                    task.time_of_day = None;
                    task.is_someday = true;
                }
                date_str => {
                    if let Ok(parsed) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                        task.start_date = Some(parsed);
                        task.time_of_day = None;
                        task.is_someday = false;
                    }
                }
            }
            let _ = self.rust().db.update_task(&task);
        }
        self.refresh();
    }

    pub fn update_task_notes(self: Pin<&mut Self>, task_id: &QString, notes: &QString) {
        let id_str = String::from(task_id);
        let notes_str = String::from(notes);
        if let Ok(Some(mut task)) = self.rust().db.get_task(&id_str) {
            task.notes = notes_str;
            let _ = self.rust().db.update_task(&task);
        }
        self.refresh();
    }

    pub fn add_checklist_item(self: Pin<&mut Self>, task_id: &QString, title: &QString) {
        let task_id_str = String::from(task_id);
        let title_str = String::from(title).trim().to_string();
        if title_str.is_empty() {
            return;
        }

        let item = ChecklistItem::new(Uuid::new_v4().to_string(), task_id_str, title_str, 0.0);
        let _ = self.rust().db.create_checklist_item(&item);
        self.refresh();
    }

    pub fn toggle_checklist_item(self: Pin<&mut Self>, item_id: &QString) {
        let id_str = String::from(item_id);
        let _ = self.rust().db.toggle_checklist_item(&id_str);
        self.refresh();
    }

    pub fn get_checklist_json(&self, task_id: &QString) -> QString {
        let id_str = String::from(task_id);
        if let Ok(items) = self.rust().db.list_checklist_items(&id_str) {
            let json = serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
            QString::from(json.as_str())
        } else {
            QString::from("[]")
        }
    }

    pub fn create_project(self: Pin<&mut Self>, title: &QString, area_id: &QString) {
        let title_str = String::from(title).trim().to_string();
        if title_str.is_empty() {
            return;
        }
        let area_id_str = String::from(area_id);
        let mut proj = Project::new(Uuid::new_v4().to_string(), title_str, 0.0);
        if !area_id_str.is_empty() {
            proj.area_id = Some(area_id_str);
        }
        let _ = self.rust().db.create_project(&proj);
        self.refresh();
    }

    pub fn create_area(self: Pin<&mut Self>, title: &QString) {
        let title_str = String::from(title).trim().to_string();
        if title_str.is_empty() {
            return;
        }
        let area = Area::new(Uuid::new_v4().to_string(), title_str, 0.0);
        let _ = self.rust().db.create_area(&area);
        self.refresh();
    }

    pub fn search(mut self: Pin<&mut Self>, query: &QString) {
        let q_str = String::from(query).trim().to_string();
        self.as_mut().set_search_query(QString::from(q_str.as_str()));
        let rust = self.rust();
        let engine = QueryEngine::new(&rust.db);

        if q_str.is_empty() {
            self.as_mut().set_search_results_json(QString::from("[]"));
        } else if let Ok(res) = engine.quick_find(&q_str) {
            let json = serde_json::to_string(&res.tasks).unwrap_or_else(|_| "[]".to_string());
            self.as_mut().set_search_results_json(QString::from(json.as_str()));
        }
    }

    pub fn refresh(mut self: Pin<&mut Self>) {
        let today = chrono::Local::now().date_naive();
        let (tasks_json, progress, in_count, td_count, up_count, any_count, some_count, log_count, projs_json, areas_json) = {
            let rust = self.rust();
            let engine = QueryEngine::new(&rust.db);

            let in_tasks = engine.inbox().unwrap_or_default();
            let td_view = engine.today(today).unwrap_or_default();
            let up_groups = engine.upcoming(today).unwrap_or_default();
            let any_tasks = engine.anytime(today).unwrap_or_default();
            let some_tasks = engine.someday_tasks().unwrap_or_default();
            let log_tasks = engine.logbook(50).unwrap_or_default();

            let in_count = in_tasks.len() as i32;
            let td_count = (td_view.daytime.len() + td_view.evening.len()) as i32;
            let up_count = up_groups.iter().map(|g| g.tasks.len()).sum::<usize>() as i32;
            let any_count = any_tasks.len() as i32;
            let some_count = some_tasks.len() as i32;
            let log_count = log_tasks.len() as i32;

            let cur_view = String::from(&rust.current_view);
            let cur_proj_id = String::from(&rust.current_project_id);

            let (tasks, prog) = match cur_view.as_str() {
                "inbox" => (serde_json::to_string(&in_tasks).unwrap_or_else(|_| "[]".to_string()), 0.0),
                "today" => {
                    // Combine daytime + evening with a flag
                    let mut combined = Vec::new();
                    for t in td_view.daytime {
                        combined.push(t);
                    }
                    for t in td_view.evening {
                        combined.push(t);
                    }
                    (serde_json::to_string(&combined).unwrap_or_else(|_| "[]".to_string()), 0.0)
                }
                "upcoming" => {
                    let mut all_up = Vec::new();
                    for g in up_groups {
                        for t in g.tasks {
                            all_up.push(t);
                        }
                    }
                    (serde_json::to_string(&all_up).unwrap_or_else(|_| "[]".to_string()), 0.0)
                }
                "anytime" => (serde_json::to_string(&any_tasks).unwrap_or_else(|_| "[]".to_string()), 0.0),
                "someday" => (serde_json::to_string(&some_tasks).unwrap_or_else(|_| "[]".to_string()), 0.0),
                "logbook" => (serde_json::to_string(&log_tasks).unwrap_or_else(|_| "[]".to_string()), 0.0),
                "project" => {
                    let p_tasks = rust.db.list_tasks_for_project(&cur_proj_id).unwrap_or_default();
                    let progress = engine.project_progress(&cur_proj_id).unwrap_or_default().ratio();
                    (serde_json::to_string(&p_tasks).unwrap_or_else(|_| "[]".to_string()), progress)
                }
                _ => ("[]".to_string(), 0.0),
            };

            let projs = rust.db.list_projects().unwrap_or_default();
            let areas = rust.db.list_areas().unwrap_or_default();

            (
                tasks,
                prog,
                in_count,
                td_count,
                up_count,
                any_count,
                some_count,
                log_count,
                serde_json::to_string(&projs).unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(&areas).unwrap_or_else(|_| "[]".to_string()),
            )
        };

        self.as_mut().set_tasks_json(QString::from(tasks_json.as_str()));
        self.as_mut().set_current_project_progress(progress);
        self.as_mut().set_inbox_count(in_count);
        self.as_mut().set_today_count(td_count);
        self.as_mut().set_upcoming_count(up_count);
        self.as_mut().set_anytime_count(any_count);
        self.as_mut().set_someday_count(some_count);
        self.as_mut().set_logbook_count(log_count);
        self.as_mut().set_projects_json(QString::from(projs_json.as_str()));
        self.as_mut().set_areas_json(QString::from(areas_json.as_str()));
    }
}
