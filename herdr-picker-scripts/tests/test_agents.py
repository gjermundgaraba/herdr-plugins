import importlib.util
import pathlib
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / "herdr-picker-agents.py"
SPEC = importlib.util.spec_from_file_location("herdr_picker_agents", SCRIPT)
AGENTS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AGENTS)


def agent(pane, status, seq, **fields):
    return {
        "pane_id": pane,
        "terminal_id": f"terminal-{pane}",
        "workspace_id": "w1",
        "agent_status": status,
        "state_change_seq": seq,
        **fields,
    }


def snapshot(*agents):
    return {
        "workspaces": [{"workspace_id": "w1", "label": "main"}],
        "agents": list(agents),
    }


class AgentsTests(unittest.TestCase):
    def test_load_snapshot_reports_launch_error(self):
        with (
            mock.patch.dict(AGENTS.os.environ, {"HERDR_BIN_PATH": "/herdr"}),
            mock.patch.object(
                AGENTS.subprocess, "run", side_effect=PermissionError("denied")
            ),
            self.assertRaisesRegex(SystemExit, "`/herdr` failed: denied"),
        ):
            AGENTS.load_snapshot()

    def test_items_order_by_status_then_recency(self):
        items = AGENTS.build_items(
            snapshot(
                agent("p-idle", "idle", 9),
                agent("p-working-old", "working", 1),
                agent("p-unknown", "surprising", 5),
                agent("p-working-new", "working", 2),
                agent("p-done", "done", 3),
                agent("p-blocked", "blocked", 4),
            )
        )

        self.assertEqual(
            [item["id"] for item in items],
            [
                "p-blocked",
                "p-done",
                "p-working-new",
                "p-working-old",
                "p-idle",
                "p-unknown",
            ],
        )

    def test_item_shape_prefers_name_and_joins_subtitle(self):
        (item,) = AGENTS.build_items(
            snapshot(
                agent(
                    "p1",
                    "working",
                    1,
                    name="refactor",
                    terminal_title="ignored",
                    cwd="/repo",
                    foreground_cwd="/repo/sub",
                    agent="claude",
                )
            )
        )

        self.assertEqual(
            item,
            {
                "id": "p1",
                "title": "main: refactor",
                "subtitle": "/repo/sub · claude",
                "badge": "claude",
                "indicator": "",
                "tone": "warning",
                "spinning": True,
                "search": "p1 terminal-p1 working",
                "value": {"pane_id": "p1"},
            },
        )

    def test_title_falls_back_and_collapses_workspace_duplicate(self):
        fallback, collapsed, unknown_workspace = AGENTS.build_items(
            snapshot(
                agent("p1", "blocked", 1),
                agent("p2", "done", 1, name="main"),
                dict(agent("p3", "idle", 1), workspace_id="w-gone"),
            )
        )

        self.assertEqual(fallback["title"], "main: terminal-p1")
        self.assertEqual(collapsed["title"], "main")
        self.assertEqual(unknown_workspace["title"], "w-gone: terminal-p3")

    def test_unknown_status_uses_the_fallback_style(self):
        (item,) = AGENTS.build_items(snapshot(agent("p1", "surprising", 1)))

        self.assertEqual(item["indicator"], "○")
        self.assertEqual(item["tone"], "muted")
        self.assertFalse(item["spinning"])


if __name__ == "__main__":
    unittest.main()
