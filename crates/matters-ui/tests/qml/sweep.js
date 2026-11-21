.pragma library

// Every state of the window a check should see (#74, #75): each view, a
// project with a heading, an area with a filed project, open tasks (one
// with a checklist), calendar events and repeat previews, the Undo notice,
// and every dialog, popover and menu. `run` calls `look(where)` in each;
// `name(text)` makes the titles of the data it creates.

// The data the states need, made once per test file.
function setup(t, view, name) {
    const controller = view.appController;
    controller.selectView("today");
    view.captureTask(name("today"));
    controller.selectView("inbox");
    view.captureTask(name("inbox"));
    controller.createArea(name("area"));
    t.wait(30);
}

// Take the data `setup` and `run` made away again: the test files share
// one database, and a longer sidebar would move what later files drag.
// `tasks`: the titles of other tasks the test file made (in the Inbox or
// Today), as `name` gives them.
function teardown(t, view, name, tasks) {
    const controller = view.appController;
    const list = view.taskList;
    const titles = [name("inbox"), name("today")].concat((tasks || []).map(name));
    for (const v of ["inbox", "today"]) {
        controller.selectView(v);
        t.wait(0);
        for (let i = list.count - 1; i >= 0; --i) {
            if (titles.indexOf(list.model.titleAt(i)) >= 0)
                controller.trashTask(list.model.idAt(i));
        }
    }
    for (const p of JSON.parse(controller.projectsJson)) {
        if (p.title === name("project") || p.title === name("filed project"))
            controller.trashProject(p.id);
    }
    for (const a of JSON.parse(controller.areasJson)) {
        if (a.title === name("area"))
            controller.trashArea(a.id);
    }
    controller.emptyTrash();
    controller.selectView("today");
    t.wait(0);
    t.verify(!JSON.parse(controller.areasJson).some(a => a.title === name("area")), "the sweep's area is gone");
    t.verify(!JSON.parse(controller.projectsJson).some(p => p.title === name("project")), "and its projects");
}

function run(t, view, look, name) {
    const controller = view.appController;
    const list = view.taskList;

    function rowFor(title) {
        for (let i = 0; i < list.count; ++i) {
            const item = list.itemAtIndex(i);
            if (item && item.title === title)
                return item;
        }
        return null;
    }

    function taskId(title) {
        t.wait(0);
        t.tryVerify(() => rowFor(title) !== null, 2000, title + " is listed");
        return rowFor(title).taskId;
    }

    function popup(objectName, open, where) {
        const p = t.findChild(view, objectName);
        t.verify(p, objectName + " exists");
        open(p);
        t.tryVerify(() => p.opened, 2000, objectName + " opens");
        look(where || objectName);
        p.close();
        t.tryVerify(() => !p.visible, 2000, objectName + " closes");
    }

    function menu(objectName, button, where) {
        const m = t.findChild(view, objectName);
        t.verify(m, objectName + " exists");
        t.mouseClick(t.findChild(view, button));
        t.tryVerify(() => m.opened, 2000, objectName + " opens");
        look(where || objectName);
        m.close();
        t.tryVerify(() => !m.visible, 2000, objectName + " closes");
    }

    for (const v of ["inbox", "today", "upcoming", "anytime", "someday", "logbook", "trash"]) {
        controller.selectView(v);
        look(v);
    }

    // A project with a heading, a task with a checklist, a repeating
    // task (previewed in Upcoming) and a finished one.
    controller.createProject(name("project"));
    const project = JSON.parse(controller.projectsJson).find(p => p.title === name("project"));
    controller.selectProject(project.id);
    controller.addHeading(name("heading"));
    const projectTask = controller.addTask(name("project task"));
    controller.addChecklistItem(projectTask, name("step"), "");
    controller.setTaskRepeat(projectTask, JSON.stringify({
        every: 1, frequency: { kind: "daily" }, after_completion: false,
        start: Qt.formatDate(new Date(), "yyyy-MM-dd")
    }));
    const done = controller.addTask(name("finished"));
    controller.toggleTask(done);
    look("project");
    list.expandedTaskId = projectTask;
    look("an open task with a checklist");
    list.expandedTaskId = "";
    menu("projectMenu", "containerMenuButton");
    view.askFinishProject("completed");
    const finish = t.findChild(view, "finishProjectDialog");
    t.tryVerify(() => finish.opened, 2000, "finishProjectDialog opens");
    look("finishProjectDialog");
    finish.close();
    t.tryVerify(() => !finish.visible, 2000);

    // A project filed in the area, listed under it in the sidebar.
    const area = JSON.parse(controller.areasJson).find(a => a.title === name("area"));
    controller.createProject(name("filed project"));
    const filed = JSON.parse(controller.projectsJson).find(p => p.title === name("filed project"));
    controller.setProjectArea(filed.id, area.id);
    controller.selectArea(area.id);
    look("area");
    menu("areaMenu", "containerMenuButton");

    controller.chooseShowEvents(true);
    controller.selectView("today");
    look("today with events");
    controller.selectView("upcoming");
    look("upcoming with events and repeats");
    controller.chooseShowEvents(false);

    controller.selectView("inbox");
    const id = taskId(name("inbox"));
    list.expandedTaskId = id;
    look("an open task");
    list.expandedTaskId = "";
    view.showUndo("task", id, name("inbox"));
    look("the Undo notice");
    view.hideUndo();

    popup("quickFindDialog", p => p.open());
    popup("preferencesDialog", p => p.open());
    popup("tagsDialog", p => p.open());
    popup("newAreaDialog", p => p.open());
    popup("newProjectDialog", p => p.open());
    popup("renameDialog", () => view.openRename("area", area.id, name("area")));
    popup("whenPopup", () => view.openWhenFor(id), "the When popover");
    popup("whenPopup", () => view.openDeadlineFor(id), "the Deadline popover");
    popup("movePopup", () => view.openMoveFor(id));
    popup("taskMenu", () => view.openTaskMenu(id, Qt.point(40, 40), list));
    popup("repeatDialog", p => p.edit([id], "", ""));
    popup("repeatDialog", p => p.edit([id], JSON.stringify({
        every: 2, frequency: { kind: "weekly", days: [1, 3] }, after_completion: false,
        start: "2026-01-05", until: "2027-01-01"
    }), "2026-01-05"), "the Repeat dialog, weekly until a day");
    popup("batchTagPopup", p => { p.taskIds = [id]; p.open(); });
    popup("openLinkDialog", () => view.openLink("file:///etc/hosts"));
    controller.selectView("trash");
    popup("emptyTrashDialog", p => p.open());
}
