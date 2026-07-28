package herdr

import "encoding/json"

const (
	TestedHerdrVersion = "0.7.5"
	TestedProtocol     = 17
)

type AgentStatus string

const (
	AgentIdle    AgentStatus = "idle"
	AgentWorking AgentStatus = "working"
	AgentBlocked AgentStatus = "blocked"
	AgentDone    AgentStatus = "done"
	AgentUnknown AgentStatus = "unknown"
)

type ServerCapabilities struct {
	LiveHandoff          bool `json:"live_handoff"`
	DetachedServerDaemon bool `json:"detached_server_daemon"`
}

type PingResult struct {
	Type         string              `json:"type"`
	Version      string              `json:"version"`
	Protocol     uint32              `json:"protocol"`
	Capabilities *ServerCapabilities `json:"capabilities,omitempty"`
}

type SessionSnapshot struct {
	Version            string               `json:"version"`
	Protocol           uint32               `json:"protocol"`
	FocusedWorkspaceID string               `json:"focused_workspace_id,omitempty"`
	FocusedTabID       string               `json:"focused_tab_id,omitempty"`
	FocusedPaneID      string               `json:"focused_pane_id,omitempty"`
	Workspaces         []WorkspaceInfo      `json:"workspaces"`
	Tabs               []TabInfo            `json:"tabs"`
	Panes              []PaneInfo           `json:"panes"`
	Layouts            []PaneLayoutSnapshot `json:"layouts"`
	Agents             []AgentInfo          `json:"agents"`
}

type WorkspaceInfo struct {
	WorkspaceID string                 `json:"workspace_id"`
	Number      uint64                 `json:"number"`
	Label       string                 `json:"label"`
	Focused     bool                   `json:"focused"`
	PaneCount   uint64                 `json:"pane_count"`
	TabCount    uint64                 `json:"tab_count"`
	ActiveTabID string                 `json:"active_tab_id"`
	AgentStatus AgentStatus            `json:"agent_status"`
	Tokens      map[string]string      `json:"tokens,omitempty"`
	Worktree    *WorkspaceWorktreeInfo `json:"worktree,omitempty"`
}

type WorkspaceWorktreeInfo struct {
	RepoKey          string `json:"repo_key"`
	RepoName         string `json:"repo_name"`
	RepoRoot         string `json:"repo_root"`
	CheckoutPath     string `json:"checkout_path"`
	IsLinkedWorktree bool   `json:"is_linked_worktree"`
}

type TabInfo struct {
	TabID       string      `json:"tab_id"`
	WorkspaceID string      `json:"workspace_id"`
	Number      uint64      `json:"number"`
	Label       string      `json:"label"`
	Focused     bool        `json:"focused"`
	PaneCount   uint64      `json:"pane_count"`
	AgentStatus AgentStatus `json:"agent_status"`
}

type AgentSessionInfo struct {
	Source string `json:"source"`
	Agent  string `json:"agent"`
	Kind   string `json:"kind"`
	Value  string `json:"value"`
}

type PaneInfo struct {
	PaneID                string            `json:"pane_id"`
	TerminalID            string            `json:"terminal_id"`
	WorkspaceID           string            `json:"workspace_id"`
	TabID                 string            `json:"tab_id"`
	Focused               bool              `json:"focused"`
	CWD                   string            `json:"cwd,omitempty"`
	ForegroundCWD         string            `json:"foreground_cwd,omitempty"`
	Label                 string            `json:"label,omitempty"`
	Agent                 string            `json:"agent,omitempty"`
	Title                 string            `json:"title,omitempty"`
	TerminalTitle         string            `json:"terminal_title,omitempty"`
	TerminalTitleStripped string            `json:"terminal_title_stripped,omitempty"`
	DisplayAgent          string            `json:"display_agent,omitempty"`
	AgentStatus           AgentStatus       `json:"agent_status"`
	StateLabels           map[string]string `json:"state_labels,omitempty"`
	Tokens                map[string]string `json:"tokens,omitempty"`
	AgentSession          *AgentSessionInfo `json:"agent_session,omitempty"`
	Scroll                *PaneScrollInfo   `json:"scroll,omitempty"`
	Revision              uint64            `json:"revision"`
}

type PaneScrollInfo struct {
	OffsetFromBottom    uint64 `json:"offset_from_bottom"`
	MaxOffsetFromBottom uint64 `json:"max_offset_from_bottom"`
	ViewportRows        uint64 `json:"viewport_rows"`
}

type AgentInfo struct {
	TerminalID             string            `json:"terminal_id"`
	Name                   string            `json:"name,omitempty"`
	Agent                  string            `json:"agent,omitempty"`
	Title                  string            `json:"title,omitempty"`
	TerminalTitle          string            `json:"terminal_title,omitempty"`
	TerminalTitleStripped  string            `json:"terminal_title_stripped,omitempty"`
	DisplayAgent           string            `json:"display_agent,omitempty"`
	AgentStatus            AgentStatus       `json:"agent_status"`
	ScreenDetectionSkipped bool              `json:"screen_detection_skipped,omitempty"`
	StateLabels            map[string]string `json:"state_labels,omitempty"`
	Tokens                 map[string]string `json:"tokens,omitempty"`
	AgentSession           *AgentSessionInfo `json:"agent_session,omitempty"`
	WorkspaceID            string            `json:"workspace_id"`
	TabID                  string            `json:"tab_id"`
	PaneID                 string            `json:"pane_id"`
	Focused                bool              `json:"focused"`
	LaunchPending          bool              `json:"launch_pending,omitempty"`
	InteractiveReady       bool              `json:"interactive_ready,omitempty"`
	StateChangeSeq         uint64            `json:"state_change_seq,omitempty"`
	CWD                    string            `json:"cwd,omitempty"`
	ForegroundCWD          string            `json:"foreground_cwd,omitempty"`
	Revision               uint64            `json:"revision"`
}

type PaneLayoutSnapshot struct {
	WorkspaceID   string            `json:"workspace_id"`
	TabID         string            `json:"tab_id"`
	Zoomed        bool              `json:"zoomed"`
	Area          PaneLayoutRect    `json:"area"`
	FocusedPaneID string            `json:"focused_pane_id"`
	Panes         []PaneLayoutPane  `json:"panes"`
	Splits        []PaneLayoutSplit `json:"splits"`
}

type PaneLayoutRect struct {
	X      uint16 `json:"x"`
	Y      uint16 `json:"y"`
	Width  uint16 `json:"width"`
	Height uint16 `json:"height"`
}

type PaneLayoutPane struct {
	PaneID  string         `json:"pane_id"`
	Focused bool           `json:"focused"`
	Rect    PaneLayoutRect `json:"rect"`
}

type PaneLayoutSplit struct {
	ID        string         `json:"id"`
	Direction string         `json:"direction"`
	Ratio     float32        `json:"ratio"`
	Rect      PaneLayoutRect `json:"rect"`
}

type LayoutNode struct {
	Type      string            `json:"type"`
	PaneID    string            `json:"pane_id,omitempty"`
	Label     string            `json:"label,omitempty"`
	CWD       string            `json:"cwd,omitempty"`
	Command   []string          `json:"command,omitempty"`
	Env       map[string]string `json:"env,omitempty"`
	Direction string            `json:"direction,omitempty"`
	Ratio     float64           `json:"ratio,omitempty"`
	First     *LayoutNode       `json:"first,omitempty"`
	Second    *LayoutNode       `json:"second,omitempty"`
}

type LayoutDescription struct {
	WorkspaceID   string      `json:"workspace_id"`
	TabID         string      `json:"tab_id"`
	Zoomed        bool        `json:"zoomed"`
	FocusedPaneID string      `json:"focused_pane_id"`
	Root          *LayoutNode `json:"root"`
}

type LayoutExportParams struct {
	TabID  string `json:"tab_id,omitempty"`
	PaneID string `json:"pane_id,omitempty"`
}

type LayoutSetSplitRatioParams struct {
	TabID  string  `json:"tab_id,omitempty"`
	PaneID string  `json:"pane_id,omitempty"`
	Path   []bool  `json:"path"`
	Ratio  float64 `json:"ratio"`
}

type EventEnvelope struct {
	Event string          `json:"event"`
	Data  json.RawMessage `json:"data"`
}

type EventSubscription struct {
	Type    string
	Filters map[string]any
}

func NewEventSubscription(eventType string) EventSubscription {
	return EventSubscription{Type: eventType}
}

func (s EventSubscription) WithFilter(name string, value any) EventSubscription {
	if s.Filters == nil {
		s.Filters = make(map[string]any)
	} else {
		copy := make(map[string]any, len(s.Filters)+1)
		for key, existing := range s.Filters {
			copy[key] = existing
		}
		s.Filters = copy
	}
	s.Filters[name] = value
	return s
}

func (s EventSubscription) MarshalJSON() ([]byte, error) {
	value := make(map[string]any, len(s.Filters)+1)
	for key, filter := range s.Filters {
		value[key] = filter
	}
	value["type"] = s.Type
	return json.Marshal(value)
}

type PluginInvocationContext struct {
	WorkspaceID       string                 `json:"workspace_id,omitempty"`
	WorkspaceLabel    string                 `json:"workspace_label,omitempty"`
	WorkspaceCWD      string                 `json:"workspace_cwd,omitempty"`
	Worktree          *WorkspaceWorktreeInfo `json:"worktree,omitempty"`
	TabID             string                 `json:"tab_id,omitempty"`
	TabLabel          string                 `json:"tab_label,omitempty"`
	FocusedPaneID     string                 `json:"focused_pane_id,omitempty"`
	FocusedPaneCWD    string                 `json:"focused_pane_cwd,omitempty"`
	FocusedPaneAgent  string                 `json:"focused_pane_agent,omitempty"`
	FocusedPaneStatus AgentStatus            `json:"focused_pane_status,omitempty"`
	SelectedText      string                 `json:"selected_text,omitempty"`
	InvocationSource  string                 `json:"invocation_source,omitempty"`
	CorrelationID     string                 `json:"correlation_id,omitempty"`
	ClickedURL        string                 `json:"clicked_url,omitempty"`
	LinkHandlerID     string                 `json:"link_handler_id,omitempty"`
}
