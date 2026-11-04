import sqlite3
from contextlib import contextmanager, nullcontext
from pathlib import Path
from types import SimpleNamespace

import pytest

from publicator_da import browser


class CalendarLocator:
    def __init__(self, page, labels=None, index=0):
        self.page = page
        self.labels = labels if labels is not None else []
        self.index = index

    @property
    def first(self):
        return self.nth(0)

    @property
    def last(self):
        return self.nth(len(self.labels) - 1)

    def nth(self, index):
        return CalendarLocator(self.page, self.labels, index)

    def count(self):
        return len(self.labels)

    def get_attribute(self, name):
        if self.page.stale_index == self.index:
            raise RuntimeError("calendar button detached")
        return self.labels[self.index] if name == "aria-label" else None

    def evaluate_all(self, expression):
        self.page.label_snapshots += 1
        return list(self.labels)

    def click(self, *args, **kwargs):
        if self.labels:
            if self.labels[self.index] == "Confirm Schedule":
                self.page.confirmed = True
            else:
                self.page.selected = self.labels[self.index]
            return
        if self.page.selector == 'button[aria-label="Go to the Next Month"]':
            self.page.next_months += 1
        elif self.page.selector == "#schedule-draft-date-picker [role=button]":
            self.page.date_picker_opened = True
        elif self.page.selector == 'button[aria-haspopup="menu"]':
            self.page.menu_opened = True
        elif self.page.selector == '[role=menuitem][label="Schedule"]':
            self.page.schedule_menu_opened = True

    def scroll_into_view_if_needed(self):
        pass


class CalendarPage:
    def __init__(self, labels):
        self.labels = labels
        self.selector = None
        self.selected = None
        self.stale_index = None
        self.label_snapshots = 0
        self.next_months = 0
        self.date_picker_opened = False
        self.menu_opened = False
        self.schedule_menu_opened = False
        self.confirmed = False

    def locator(self, selector):
        self.selector = selector
        if selector == "#schedule-draft-date-picker button[aria-label]":
            labels = self.labels
        else:
            labels = []
        return CalendarLocator(self, labels)

    def select_option(self, *args, **kwargs):
        pass

    def wait_for_timeout(self, milliseconds):
        pass

    def get_by_text(self, text, **kwargs):
        if text == "Confirm Schedule":
            return CalendarLocator(self, ["Confirm Schedule"])
        return CalendarLocator(self)

def test_schedule_chooses_the_requested_full_year_and_exact_day():
    page = CalendarPage([
        "Wednesday, January 1, 2025",
        "Thursday, January 1, 2026",
        "Saturday, January 10, 2026",
    ])
    browser._pick_schedule(page, {"year": 2026, "month": 1, "day": 1, "hour": 20})
    assert page.selected == "Thursday, January 1, 2026"
    assert page.confirmed


def test_schedule_snapshots_calendar_labels_instead_of_waiting_on_each_button():
    labels = ["Tuesday, November 10, 2026", *([""] * 32)]
    page = CalendarPage(labels)
    page.stale_index = 32

    browser._pick_schedule(page, {"year": 2026, "month": 11, "day": 10, "hour": 20})

    assert page.selected == labels[0]
    assert page.label_snapshots == 1
    assert page.confirmed


def test_schedule_refuses_unqualified_calendar_dates():
    page = CalendarPage(["January 15", "January 15"])
    with pytest.raises(RuntimeError, match="full requested date"):
        browser._pick_schedule(page, {"year": 2026, "month": 1, "day": 15, "hour": 20})
    assert page.selected is None
    assert not page.confirmed


def test_profile_trees_must_be_disjoint(tmp_path):
    source = tmp_path / ".deviantart-login"
    work = tmp_path / ".deviantart-session"
    source.mkdir()
    work.mkdir()
    browser.validate_profiles(tmp_path, source, work)
    for a, b in ((source, source), (source, source / "nested"), (source / "nested", source)):
        with pytest.raises(ValueError):
            browser.validate_profiles(tmp_path, a, b)


def test_foreign_or_symlink_work_profile_is_rejected_untouched(tmp_path, monkeypatch):
    import sys
    from types import ModuleType, SimpleNamespace

    source = tmp_path / ".deviantart-login"
    source.mkdir()
    (source / "prefs.js").write_text("login")
    foreign = tmp_path / "unrelated"
    foreign.mkdir()
    sentinel = foreign / "sentinel"
    sentinel.write_text("keep")
    work = foreign
    work_link = tmp_path / ".deviantart-session"
    work_link.symlink_to(foreign, target_is_directory=True)
    calls = []
    module = ModuleType("playwright.sync_api")
    module.sync_playwright = lambda: calls.append("acquire") or SimpleNamespace(start=lambda: None)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", module)

    for work_profile in (work, work_link):
        with pytest.raises(ValueError):
            browser.BrowserSession({
                "login_profile": str(source), "work_profile": str(work_profile),
                "headless": True, "timezone": "Europe/Paris",
            }, tmp_path)
        assert sentinel.read_text() == "keep"
        assert list(foreign.iterdir()) == [sentinel]
    assert calls == []


def test_browser_session_rejects_same_name_foreign_root_before_playwright(tmp_path, monkeypatch):
    from types import ModuleType, SimpleNamespace

    trusted = tmp_path / "trusted"
    trusted.mkdir()
    source = tmp_path / "foreign" / ".deviantart-login"
    work = tmp_path / "foreign" / ".deviantart-session"
    work.parent.mkdir()
    source.mkdir()
    work.mkdir()
    sentinel = work / "sentinel"
    sentinel.write_text("keep")
    acquired = []
    module = ModuleType("playwright.sync_api")
    module.sync_playwright = lambda: acquired.append(True) or SimpleNamespace(start=lambda: None)
    monkeypatch.setitem(__import__("sys").modules, "playwright.sync_api", module)

    with pytest.raises(ValueError):
        browser.BrowserSession({
            "login_profile": str(source), "work_profile": str(work),
            "headless": True, "timezone": "Europe/Paris",
        }, trusted)
    assert acquired == []
    assert sentinel.read_text() == "keep"
    assert sorted(path.name for path in work.iterdir()) == ["sentinel"]


def test_work_profile_copy_dereferences_symlinks_and_skips_stale_lock(tmp_path, monkeypatch):
    source = tmp_path / ".deviantart-login"
    work = tmp_path / ".deviantart-session"
    external = tmp_path / "outside.sqlite"
    source.mkdir()
    external.write_text("source")
    (source / "linked.sqlite").symlink_to(external)
    (source / "lock").symlink_to(tmp_path / "missing-lock-target")
    # Copy behavior is exercised through session construction after Playwright is faked.
    import sys
    from types import ModuleType, SimpleNamespace
    fake_page = SimpleNamespace(goto=lambda *a, **k: None, wait_for_timeout=lambda ms: None, url="/studio")
    fake_context = SimpleNamespace(pages=[fake_page], new_page=lambda: fake_page,
                                   add_cookies=lambda cookies: None, close=lambda: None)
    fake_playwright = SimpleNamespace(firefox=SimpleNamespace(
        launch_persistent_context=lambda *a, **k: fake_context), stop=lambda: None)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", ModuleType("playwright.sync_api"))
    sys.modules["playwright.sync_api"].sync_playwright = lambda: SimpleNamespace(start=lambda: fake_playwright)
    monkeypatch.setattr(browser.BrowserSession, "_cookies", lambda self: [])
    session = browser.BrowserSession({"login_profile": str(source), "work_profile": str(work),
                                     "headless": True, "timezone": "Europe/Paris"}, tmp_path)
    try:
        copied = work / "linked.sqlite"
        assert not copied.is_symlink()
        copied.write_text("work")
        assert external.read_text() == "source"
        assert not (work / "lock").is_symlink()
    finally:
        session.close()


def test_browser_without_cookie_database_passes_no_cookies(tmp_path, monkeypatch):
    import sys
    from types import ModuleType

    source, work = tmp_path / ".deviantart-login", tmp_path / ".deviantart-session"
    source.mkdir()
    (source / "prefs.js").write_text("disposable profile")
    calls = {}

    class Page:
        url = "https://www.deviantart.com/studio?new=1"
        def goto(self, *args, **kwargs): pass
        def wait_for_timeout(self, ms): pass

    class Context:
        pages = [Page()]
        def add_cookies(self, cookies): calls["cookies"] = cookies
        def close(self): pass

    context = Context()
    playwright = SimpleNamespace(
        firefox=SimpleNamespace(
            launch_persistent_context=lambda *args, **kwargs: context
        ),
        stop=lambda: None,
    )
    monkeypatch.setitem(sys.modules, "playwright.sync_api", ModuleType("playwright.sync_api"))
    sys.modules["playwright.sync_api"].sync_playwright = lambda: SimpleNamespace(
        start=lambda: playwright
    )

    session = browser.BrowserSession({
        "login_profile": str(source),
        "work_profile": str(work),
        "headless": True,
        "timezone": "Europe/Paris",
    }, tmp_path)
    try:
        assert calls["cookies"] == []
    finally:
        session.close()


def tree_bytes(root):
    return {path: path.read_bytes() for path in sorted(root.rglob("*")) if path.is_file()}


def test_browser_setup_passes_headless_timezone_and_reinjects_cookies(tmp_path, monkeypatch):
    source, work = tmp_path / ".deviantart-login", tmp_path / ".deviantart-session"
    source.mkdir()
    db = sqlite3.connect(source / "cookies.sqlite")
    db.execute("create table moz_cookies (name,value,host,path,isSecure,isHttpOnly)")
    db.execute("insert into moz_cookies values ('auth','secret','.deviantart.com','/',1,1)")
    db.commit(); db.close()
    calls = {}

    class Page:
        url = "https://www.deviantart.com/studio?new=1"
        def goto(self, url, **kwargs): calls["goto"] = (url, kwargs)
        def wait_for_timeout(self, ms): pass

    page = Page()
    class Context:
        pages = [page]
        def add_cookies(self, cookies): calls["cookies"] = cookies
        def close(self): pass
    context = Context()
    class Playwright:
        firefox = self = None
        def launch_persistent_context(self, path, **kwargs):
            calls["launch"] = (path, kwargs); return context
        def stop(self): pass
    pw = Playwright(); pw.firefox = pw
    from types import ModuleType, SimpleNamespace
    monkeypatch.setitem(__import__("sys").modules, "playwright.sync_api", ModuleType("playwright.sync_api"))
    __import__("sys").modules["playwright.sync_api"].sync_playwright = lambda: SimpleNamespace(start=lambda: pw)

    original = tree_bytes(source)
    session = browser.BrowserSession({"login_profile": str(source), "work_profile": str(work),
                                     "headless": True, "timezone": "Europe/Paris"}, tmp_path)
    session.close()
    assert tree_bytes(source) == original, "the login profile is never modified"
    assert calls["launch"][1] == {"headless": True, "timezone_id": "Europe/Paris"}
    assert calls["cookies"] == [{"name": "auth", "value": "secret", "domain": ".deviantart.com",
                                  "path": "/", "secure": True, "httpOnly": True}]
    assert calls["goto"][0] == "https://www.deviantart.com/studio?new=1"
    assert (source / "cookies.sqlite").is_file()


def test_initialization_failure_closes_partial_browser_resources(tmp_path, monkeypatch):
    source, work = tmp_path / ".deviantart-login", tmp_path / ".deviantart-session"
    source.mkdir(); (source / "profile").write_text("x")
    calls = []
    class Context:
        pages = []
        def new_page(self): raise RuntimeError("page failed")
        def close(self): calls.append("context.close")
    context = Context()
    class Playwright:
        firefox = self = None
        def launch_persistent_context(self, *a, **kw): return context
        def stop(self): calls.append("playwright.stop")
    pw = Playwright(); pw.firefox = pw
    from types import ModuleType, SimpleNamespace
    monkeypatch.setitem(__import__("sys").modules, "playwright.sync_api", ModuleType("playwright.sync_api"))
    __import__("sys").modules["playwright.sync_api"].sync_playwright = lambda: SimpleNamespace(start=lambda: pw)
    monkeypatch.setattr(browser.BrowserSession, "_cookies", lambda self: [])
    with pytest.raises(RuntimeError, match="page failed"):
        browser.BrowserSession({"login_profile": str(source), "work_profile": str(work),
                               "headless": False, "timezone": "Europe/Paris"}, tmp_path)
    assert calls == ["context.close", "playwright.stop"]


def test_steps_parser_detects_skill_drift(tmp_path):
    skill = tmp_path / "SKILL.md"
    skill.write_text("### Steps\n1. Connect to site\n2. Submit\n\n### Other\n")
    assert browser.check_steps(skill, ["Connect to site", "Submit"])
    assert not browser.check_steps(skill, ["Connect to site"])


DEVIATION = "https://www.deviantart.com/artist/art/Title-1"


def reply(url=browser.COMMIT_URL, method="POST", body={"status": "success", "url": DEVIATION}):
    return SimpleNamespace(url=url, request=SimpleNamespace(method=method), json=lambda: body)


JOB = {"attempt_id": "attempt", "uuid": "artwork", "image_path": "art.png", "title": "Title",
       "description": "Description", "tags": ["new"], "schedule": {}, "tiers": [], "galleries": []}


class CheckboxPage:
    """Per-name checkbox that reads checked once it has been clicked `sticks_after` times."""

    def __init__(self, sticks_after=1):
        self.sticks_after, self.clicks = sticks_after, {}

    def evaluate(self, expression, name):
        if expression.endswith(".click()"):
            self.clicks[name] = self.clicks.get(name, 0) + 1
            return None
        return self.sticks_after is not None and self.clicks.get(name, 0) >= self.sticks_after

    def wait_for_timeout(self, ms):
        pass


@pytest.mark.parametrize("sticks_after", [0, 1, 2, 3])
def test_checkbox_returns_once_state_sticks_including_after_the_last_click(sticks_after):
    page = CheckboxPage(sticks_after)
    browser.set_checkbox(page, "matureContent", True)
    assert page.clicks.get("matureContent", 0) == sticks_after
    assert page.evaluate("n => input.checked", "matureContent") is True


def test_checkbox_that_never_sticks_raises_after_three_clicks_and_blocks_commit(monkeypatch):
    page = CheckboxPage(None)
    with pytest.raises(RuntimeError, match="would not stay True"):
        browser.set_checkbox(page, "matureContent", True)
    assert page.clicks == {"matureContent": 3}
    form = FormPage(sticks_after=None)
    assert (submit(monkeypatch, form)["type"], form.commits) == ("failed", 0)


class FormLocator:
    first = last = property(lambda self: self)

    def __init__(self, page, kind, text=""):
        self.page, self.kind, self.text = page, kind, text

    def click(self, *args, **kwargs): self.page.clicked(self)
    def fill(self, value): pass
    def type(self, value, delay=0): self.page.typed.append(value)
    def set_files(self, path): pass
    def count(self): return self.page.chips
    def scroll_into_view_if_needed(self): pass
    def locator(self, selector): return FormLocator(self.page, "tiers")
    def evaluate(self, expression): return list(self.page.selected[self.kind])


class FormPage(CheckboxPage):
    """One stateful submit form whose Schedule click is answered by `answer` (None: no reply; an exception is raised)."""

    def __init__(self, answer=reply(), sticks_after=1, chips=0, chip_click="removes", galleries=(), sticky=True):
        super().__init__(sticks_after)
        self.answer = answer
        self.chips, self.chip_click = chips, chip_click
        self.selected, self.sticky, self.open_combo = {"tiers": [], "gallery": list(galleries)}, sticky, None
        self.typed, self.asked, self.picks, self.commits = [], [], [], 0
        self.keyboard = SimpleNamespace(press=lambda key: None)

    def goto(self, url, **kwargs): pass
    def wait_for_load_state(self, *args, **kwargs): pass
    def expect_file_chooser(self, **kwargs): return nullcontext(SimpleNamespace(value=FormLocator(self, "file")))
    def get_by_role(self, role, **kwargs): return FormLocator(self, "link")
    def get_by_label(self, label, **kwargs): return self.ask("label", label)
    def get_by_text(self, text, **kwargs): return self.ask("text", text)
    def locator(self, selector):
        kind = "chips" if "data-tag" in selector else "gallery" if "Gallery" in selector else "field"
        return self.ask(kind, selector)

    def ask(self, kind, text):
        self.asked.append(text)
        return FormLocator(self, kind, text)

    def clicked(self, locator):
        if locator.kind == "link":
            pass
        elif locator.kind == "chips":
            if self.chip_click == "raises":
                raise RuntimeError("chip click timed out")
            if self.chip_click == "removes":
                self.chips -= 1
        elif locator.kind in self.selected:
            self.open_combo = None if self.open_combo else locator.kind
        elif locator.text == "Schedule":
            self.commits += 1
        elif self.open_combo:
            self.pick(self.selected[self.open_combo], locator.text)

    def pick(self, labels, label):
        """Clicking an option toggles it, like the live multi-select; a non-sticky form ignores it."""
        self.picks.append(label)
        if self.sticky:
            if label in labels:
                labels.remove(label)
            else:
                labels.append(label)

    @contextmanager
    def expect_response(self, accepted, timeout):
        info = SimpleNamespace()
        yield info
        if isinstance(self.answer, Exception):
            raise self.answer
        if not (self.commits and timeout == browser.COMMIT_EVIDENCE_MS and self.answer and accepted(self.answer)):
            raise TimeoutError(f'Timeout {timeout}ms exceeded while waiting for event "response"')
        info.value = self.answer


def submit(monkeypatch, page, pick_schedule=lambda *args: None, **changes):
    monkeypatch.setattr(browser, "_pick_schedule", pick_schedule)
    return browser.BrowserSession.submit(SimpleNamespace(page=page), {**JOB, **changes})


@pytest.mark.parametrize("answer, outcome, url", [
    (reply(), "scheduled", DEVIATION),
    (reply(url=browser.COMMIT_URL + "?csrf=1"), "scheduled", DEVIATION),
    (reply(body={"status": "success"}), "scheduled", None),
    (None, "unknown", None),  # validation rejected the form: nothing was sent
    (reply(body={"status": "error"}), "unknown", None),
    (reply(method="GET"), "unknown", None),
    (reply(url="https://elsewhere.example/_napi/shared_api/deviation/update"), "unknown", None),
    (RuntimeError("page crashed"), "unknown", None),  # exception after the Schedule click
])
def test_only_a_confirmed_commit_reply_is_scheduled(monkeypatch, answer, outcome, url):
    page = FormPage(answer=answer)
    event = submit(monkeypatch, page)
    assert (event["type"], event.get("url"), page.commits) == (outcome, url, 1)
    assert ("error" in event) == (outcome == "unknown")


def test_exception_before_commit_is_failed_and_never_clicks_schedule(monkeypatch):
    def rejected(*args):
        raise RuntimeError("calendar did not expose the full requested date")
    page = FormPage()
    event = submit(monkeypatch, page, pick_schedule=rejected)
    assert (event["type"], page.commits) == ("failed", 0)


def test_submit_selects_every_requested_tier(monkeypatch):
    page = FormPage()
    assert submit(monkeypatch, page, tiers=["gold", "silver"])["type"] == "scheduled"
    assert page.selected["tiers"] == ["gold", "silver"]


def test_submit_skips_unset_premium_tiers_and_galleries(monkeypatch):
    page = FormPage()
    assert submit(monkeypatch, page)["type"] == "scheduled"
    assert not {"Sell as Premium Download", "Submit to your subscribers"} & set(page.asked)
    assert not any("Gallery" in asked for asked in page.asked)
    assert page.typed == ["new"]


@pytest.mark.parametrize("chips, chip_click, outcome", [
    (3, "removes", "scheduled"),
    (60, "removes", "scheduled"),
    (61, "removes", "failed"),  # more old tags than the bounded loop clears
    (2, "sticks", "failed"),  # clicks make no progress
    (2, "raises", "failed"),
])
def test_pre_filled_tags_must_all_clear_before_new_tags_or_commit(monkeypatch, chips, chip_click, outcome):
    page = FormPage(chips=chips, chip_click=chip_click)
    assert submit(monkeypatch, page)["type"] == outcome
    cleared = outcome == "scheduled"
    assert (page.chips == 0, page.typed, page.commits) == (cleared, ["new"] if cleared else [], int(cleared))


@pytest.mark.parametrize("selected, requested, picks", [
    (["Artwork"], ["Art"], ["Art"]),  # a selected superstring does not satisfy the request
    (["Art"], ["Art"], []),  # an exact selection is not toggled off
    ([], ["Art", "Artwork"], ["Art", "Artwork"]),
    (["Artwork", "Art"], ["Art", "Artwork"], []),
])
def test_galleries_compare_exact_selected_labels(monkeypatch, selected, requested, picks):
    page = FormPage(galleries=selected)
    assert submit(monkeypatch, page, galleries=requested)["type"] == "scheduled"
    assert page.picks == picks
    assert set(requested) <= set(page.selected["gallery"])


def test_gallery_pick_that_does_not_stick_fails_before_commit(monkeypatch):
    page = FormPage(sticky=False)
    event = submit(monkeypatch, page, galleries=["Art"])
    assert (event["type"], page.picks, page.commits) == ("failed", ["Art"], 0)
