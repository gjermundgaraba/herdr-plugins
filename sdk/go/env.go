package herdr

import (
	"encoding/json"
	"fmt"
	"os"
)

type Environment struct {
	IsHerdr         bool
	SocketPath      string
	BinPath         string
	PluginID        string
	PluginRoot      string
	PluginConfigDir string
	PluginStateDir  string
	Context         *PluginInvocationContext
	WorkspaceID     string
	TabID           string
	PaneID          string
	ActionID        string
	EventName       string
	Event           *EventEnvelope
	EntrypointID    string
	ClickedURL      string
	LinkHandlerID   string
}

func LoadEnvironment() (Environment, error) {
	var environment Environment
	environment.IsHerdr = os.Getenv("HERDR_ENV") == "1"
	environment.SocketPath = os.Getenv("HERDR_SOCKET_PATH")
	environment.BinPath = os.Getenv("HERDR_BIN_PATH")
	environment.PluginID = os.Getenv("HERDR_PLUGIN_ID")
	environment.PluginRoot = os.Getenv("HERDR_PLUGIN_ROOT")
	environment.PluginConfigDir = os.Getenv("HERDR_PLUGIN_CONFIG_DIR")
	environment.PluginStateDir = os.Getenv("HERDR_PLUGIN_STATE_DIR")
	environment.WorkspaceID = os.Getenv("HERDR_WORKSPACE_ID")
	environment.TabID = os.Getenv("HERDR_TAB_ID")
	environment.PaneID = os.Getenv("HERDR_PANE_ID")
	environment.ActionID = os.Getenv("HERDR_PLUGIN_ACTION_ID")
	environment.EventName = os.Getenv("HERDR_PLUGIN_EVENT")
	environment.EntrypointID = os.Getenv("HERDR_PLUGIN_ENTRYPOINT_ID")
	environment.ClickedURL = os.Getenv("HERDR_PLUGIN_CLICKED_URL")
	environment.LinkHandlerID = os.Getenv("HERDR_PLUGIN_LINK_HANDLER_ID")

	if err := decodeEnvironmentJSON("HERDR_PLUGIN_CONTEXT_JSON", &environment.Context); err != nil {
		return Environment{}, err
	}
	if err := decodeEnvironmentJSON("HERDR_PLUGIN_EVENT_JSON", &environment.Event); err != nil {
		return Environment{}, err
	}
	return environment, nil
}

func decodeEnvironmentJSON(name string, target any) error {
	value := os.Getenv(name)
	if value == "" {
		return nil
	}
	if err := json.Unmarshal([]byte(value), target); err != nil {
		return fmt.Errorf("%s contains invalid JSON: %w", name, err)
	}
	return nil
}
