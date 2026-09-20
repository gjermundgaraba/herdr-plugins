import importlib.util
import io
import json
import os
import pathlib
import unittest
from contextlib import redirect_stdout
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / "herdr-picker-move-pane.py"
SPEC = importlib.util.spec_from_file_location("herdr_picker_move_pane", SCRIPT)
MOVE_PANE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MOVE_PANE)


class MovePaneTests(unittest.TestCase):
    def test_one_tab_moves_directly_without_starting_picker(self):
        with (
            mock.patch.object(MOVE_PANE, "list_tabs", return_value=[{"tab_id": "w1:t1"}]),
            mock.patch.object(MOVE_PANE, "move_to_new_tab") as move,
            mock.patch.object(MOVE_PANE.os, "execvp") as execvp,
        ):
            MOVE_PANE.launch()

        move.assert_called_once_with()
        execvp.assert_not_called()

    def test_multiple_tabs_start_the_picker(self):
        with (
            mock.patch.object(
                MOVE_PANE,
                "list_tabs",
                return_value=[{"tab_id": "w1:t1"}, {"tab_id": "w1:t2"}],
            ),
            mock.patch.object(MOVE_PANE.os, "execvp") as execvp,
        ):
            MOVE_PANE.launch()

        execvp.assert_called_once_with(
            "herdr-picker", ["herdr-picker", "run", "move-pane"]
        )

    def test_new_tab_move_uses_the_active_pane_and_workspace(self):
        with (
            mock.patch.dict(
                os.environ,
                {
                    "HERDR_ACTIVE_PANE_ID": "w1:p1",
                    "HERDR_ACTIVE_WORKSPACE_ID": "w1",
                },
            ),
            mock.patch.object(MOVE_PANE, "herdr_command") as herdr,
        ):
            MOVE_PANE.move_to_new_tab()

        herdr.assert_called_once_with(
            "pane",
            "move",
            "w1:p1",
            "--new-tab",
            "--workspace",
            "w1",
            "--focus",
        )

    def test_tab_list_captures_stdout_but_inherits_stderr(self):
        response = mock.Mock(stdout='{"result":{"tabs":[]}}')
        with (
            mock.patch.dict(
                os.environ,
                {
                    "HERDR_ACTIVE_WORKSPACE_ID": "w1",
                    "HERDR_BIN_PATH": "/herdr",
                },
            ),
            mock.patch.object(MOVE_PANE.subprocess, "run", return_value=response) as run,
        ):
            self.assertEqual(MOVE_PANE.list_tabs(), [])

        run.assert_called_once_with(
            ["/herdr", "tab", "list", "--workspace", "w1"],
            check=True,
            text=True,
            stdout=MOVE_PANE.subprocess.PIPE,
        )

    def test_source_puts_new_tab_first_and_excludes_active_tab(self):
        tabs = [
            {
                "tab_id": "w1:t2",
                "number": 2,
                "label": "logs",
                "pane_count": 2,
                "agent_status": "idle",
            },
            {
                "tab_id": "w1:t1",
                "number": 1,
                "label": "main",
                "pane_count": 1,
                "agent_status": "working",
            },
        ]
        output = io.StringIO()
        with (
            mock.patch.dict(os.environ, {"HERDR_ACTIVE_TAB_ID": "w1:t1"}),
            mock.patch.object(MOVE_PANE, "list_tabs", return_value=tabs),
            mock.patch.object(MOVE_PANE.sys, "stdin", io.StringIO("{}")),
            redirect_stdout(output),
        ):
            MOVE_PANE.source()

        items = json.loads(output.getvalue())["items"]
        self.assertEqual([item["id"] for item in items], ["new-tab", "w1:t2"])
        self.assertNotIn("badge", items[1])

    def test_existing_tab_submit_moves_right_and_focuses(self):
        context = {
            "step": "destination",
            "selections": {
                "destination": {"value": {"type": "tab", "tab_id": "w1:t2"}}
            },
        }
        with (
            mock.patch.dict(os.environ, {"HERDR_ACTIVE_PANE_ID": "w1:p1"}),
            mock.patch.object(MOVE_PANE.sys, "stdin", io.StringIO(json.dumps(context))),
            mock.patch.object(MOVE_PANE, "herdr_command") as herdr,
        ):
            MOVE_PANE.submit()

        herdr.assert_called_once_with(
            "pane",
            "move",
            "w1:p1",
            "--tab",
            "w1:t2",
            "--split",
            "right",
            "--focus",
        )


if __name__ == "__main__":
    unittest.main()
