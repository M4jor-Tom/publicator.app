"""Offline DOM contract for the submit session: a routed local form in real headless Firefox.

Proves the session logic against real DOM semantics; live DeviantArt selectors stay unproven.
"""
import json
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urlsplit

import pytest

from publicator_da import browser

sync_api = pytest.importorskip("playwright.sync_api")
FORM_HTML = Path(__file__).with_name("submit_form.html").read_text(encoding="utf-8")
PARIS_2027 = {"epoch": 1800039600, "timezone": "Europe/Paris", "year": 2027, "month": 1, "day": 15, "hour": 20, "minute": 0}
DEVIATION = "https://www.deviantart.com/artist/art/Title-1"
JOB = {"attempt_id": "attempt", "uuid": "artwork", "title": "Title", "description": "Description",
       "tags": ["fresh"], "tiers": ["Gold", "Silver"], "galleries": ["Art", "Featured"], "schedule": PARIS_2027}


def serve_offline(route):
    # Only the stand-in origin is answered, from memory; every other request is refused.
    url = urlsplit(route.request.url)
    if url.hostname != "www.deviantart.com":
        route.abort()
    elif url.path == urlsplit(browser.COMMIT_URL).path:
        refused = route.request.post_data_json["outcome"] == "refused"
        route.fulfill(json={"status": "error"} if refused else {"status": "success", "url": DEVIATION})
    else:
        route.fulfill(status=200, content_type="text/html", body=FORM_HTML)


@pytest.fixture(scope="module")
def firefox():
    with sync_api.sync_playwright() as playwright:
        try:
            # A dead loopback proxy refuses anything that escapes the context routes.
            engine = playwright.firefox.launch(headless=True, timeout=60000, proxy={"server": "http://127.0.0.1:9"})
        except Exception as exc:
            pytest.skip(f"offline headless Firefox could not launch: {exc}")
        yield engine
        engine.close()


@pytest.fixture
def open_form(firefox):
    contexts = []

    def open_form(timezone="Europe/Paris", **fixture):
        context = firefox.new_context(timezone_id=timezone)
        contexts.append(context)
        context.set_default_timeout(10000)  # generous for loaded hosts; only the nonexistent-hour test waits it out
        context.add_init_script(f"window.FIXTURE = {json.dumps(fixture)};")
        context.route("**/*", serve_offline)
        return context.new_page()

    yield open_form
    for context in contexts:
        context.close()


def storage(page):
    page.wait_for_load_state()
    return page.evaluate("() => Object.fromEntries(Object.entries(sessionStorage).map(([k, v]) => [k, JSON.parse(v)]))")


def submit(page, tmp_path, **changes):
    image = tmp_path / "art.png"
    image.write_bytes(b"\x89PNG\r\n\x1a\n")
    return browser.BrowserSession.submit(SimpleNamespace(page=page), {
        **JOB, "image_path": str(image), **changes,
    })


@pytest.mark.parametrize("outcome, expected", [
    ("land", "scheduled"), ("delayed", "scheduled"), ("reject", "unknown"), ("refused", "unknown")])
def test_submit_is_scheduled_only_on_a_confirmed_commit_reply(open_form, tmp_path, monkeypatch, outcome, expected):
    monkeypatch.setattr(browser, "COMMIT_EVIDENCE_MS", 3000)
    # Selected "Artwork" must not satisfy "Art"; selected "Featured" and "Gold" must not be toggled off.
    page = open_form(start=[2026, 11], tags=["stale", "old"], selected=["Artwork", "Featured"],
                     offered=["Art", "Artwork", "Featured"], selected_tiers=["Gold"],
                     offered_tiers=["Gold", "Silver"], outcome=outcome)
    event = submit(page, tmp_path)
    assert (event["type"], event.get("url")) == (expected, DEVIATION if expected == "scheduled" else None)
    state = storage(page)
    assert state["commits"] == 1
    if expected == "scheduled":
        assert state["submitted"] == {
            "scheduled": {"date": [2027, 1, 15], "hour": 20, "epoch": 1800039600, "timezone": "Europe/Paris"},
            "tiers": ["Gold", "Silver"], "galleries": ["Artwork", "Featured", "Art"], "tags": ["fresh"], "files": 1,
            "mature": True, "ai": True,
        }


def test_submit_fails_without_commit_when_a_checkbox_will_not_stick(open_form, tmp_path):
    page = open_form(start=[2026, 11], offered=["Art", "Featured"], offered_tiers=["Gold", "Silver"],
                     stuck=["isAiGenerated"])
    event = submit(page, tmp_path)
    assert (event["type"], "would not stay True" in event["error"]) == ("failed", True)
    assert "commits" not in storage(page)


def pick(page, timezone, year, month, day, hour):
    page.goto("https://www.deviantart.com/submit")
    browser._pick_schedule(page, {"timezone": timezone, "year": year, "month": month, "day": day, "hour": hour})


def test_combo_selection_ignores_same_label_outside_open_listbox(open_form, tmp_path):
    tiers = ["Exclusive Content", "Patreon Time Traveler"]
    page = open_form(decoys=tiers, offered_tiers=tiers)

    event = submit(page, tmp_path, tiers=tiers, galleries=[])

    assert event["type"] == "scheduled", event
    assert storage(page)["submitted"]["tiers"] == tiers


@pytest.mark.parametrize("timezone, start, epoch, wall", [
    # A fold hour occurs twice; jobs carry its first occurrence (Rust min(), zoneinfo fold=0), the
    # only instant an hour picker can express. Same vector as tests/publish_protocol.rs.
    ("America/New_York", [2026, 10], 1793509200, (2026, 11, 1, 1)),
    # Rust renders the spring-forward instant as 03:00, the first hour that exists that night.
    ("Europe/Paris", [2026, 3], 1774746000, (2026, 3, 29, 3)),
])
def test_schedule_picker_reads_back_the_intended_instant(open_form, timezone, start, epoch, wall):
    page = open_form(timezone=timezone, start=start)
    pick(page, timezone, *wall)
    assert storage(page)["scheduled"] == {"date": list(wall[:3]), "hour": wall[3], "epoch": epoch, "timezone": timezone}


def test_schedule_picker_refuses_a_nonexistent_hour(open_form):
    page = open_form(start=[2026, 3])
    with pytest.raises(sync_api.TimeoutError, match="select_option"):
        pick(page, "Europe/Paris", 2026, 3, 29, 2)
    assert "scheduled" not in storage(page)
