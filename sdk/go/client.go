package herdr

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"sync/atomic"
	"time"
)

var ErrMissingSocketPath = errors.New("HERDR_SOCKET_PATH is not set")

type Client struct {
	SocketPath string
	Timeout    time.Duration
	nextID     atomic.Uint64
}

func New(socketPath string) *Client {
	return &Client{SocketPath: socketPath}
}

func NewFromEnv() (*Client, error) {
	socketPath := os.Getenv("HERDR_SOCKET_PATH")
	if socketPath == "" {
		return nil, ErrMissingSocketPath
	}
	return New(socketPath), nil
}

type APIError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

func (e *APIError) Error() string {
	return e.Code + ": " + e.Message
}

type request struct {
	ID     string `json:"id"`
	Method string `json:"method"`
	Params any    `json:"params"`
}

type response struct {
	ID     string          `json:"id"`
	Result json.RawMessage `json:"result"`
	Error  *APIError       `json:"error"`
}

// Call invokes any Herdr socket method. Result must be a pointer or nil.
func (c *Client) Call(ctx context.Context, method string, params, result any) error {
	id := c.requestID()
	conn, err := c.connect(ctx)
	if err != nil {
		return err
	}
	defer conn.Close()

	stop, err := applyContext(ctx, conn, c.Timeout)
	if err != nil {
		return err
	}
	defer stop()

	if err := writeRequest(conn, request{ID: id, Method: method, Params: nonnilParams(params)}); err != nil {
		return err
	}
	wire, err := readResponse(bufio.NewReader(conn), id)
	if err != nil {
		return err
	}
	if result == nil {
		return nil
	}
	if err := json.Unmarshal(wire, result); err != nil {
		return fmt.Errorf("decode Herdr result: %w", err)
	}
	return nil
}

func (c *Client) Ping(ctx context.Context) (PingResult, error) {
	var result PingResult
	err := c.Call(ctx, "ping", nil, &result)
	return result, err
}

func (c *Client) Snapshot(ctx context.Context) (SessionSnapshot, error) {
	var result struct {
		Type     string          `json:"type"`
		Snapshot SessionSnapshot `json:"snapshot"`
	}
	err := c.Call(ctx, "session.snapshot", nil, &result)
	return result.Snapshot, err
}

func (c *Client) Workspaces(ctx context.Context) ([]WorkspaceInfo, error) {
	var result struct {
		Type       string          `json:"type"`
		Workspaces []WorkspaceInfo `json:"workspaces"`
	}
	err := c.Call(ctx, "workspace.list", nil, &result)
	return result.Workspaces, err
}

func (c *Client) Tabs(ctx context.Context, workspaceID string) ([]TabInfo, error) {
	params := map[string]any{}
	if workspaceID != "" {
		params["workspace_id"] = workspaceID
	}
	var result struct {
		Type string    `json:"type"`
		Tabs []TabInfo `json:"tabs"`
	}
	err := c.Call(ctx, "tab.list", params, &result)
	return result.Tabs, err
}

func (c *Client) Panes(ctx context.Context, workspaceID string) ([]PaneInfo, error) {
	params := map[string]any{}
	if workspaceID != "" {
		params["workspace_id"] = workspaceID
	}
	var result struct {
		Type  string     `json:"type"`
		Panes []PaneInfo `json:"panes"`
	}
	err := c.Call(ctx, "pane.list", params, &result)
	return result.Panes, err
}

func (c *Client) CurrentPane(ctx context.Context, callerPaneID string) (PaneInfo, error) {
	params := map[string]any{}
	if callerPaneID != "" {
		params["caller_pane_id"] = callerPaneID
	}
	return c.paneResult(ctx, "pane.current", params)
}

func (c *Client) Pane(ctx context.Context, paneID string) (PaneInfo, error) {
	return c.paneResult(ctx, "pane.get", map[string]any{"pane_id": paneID})
}

func (c *Client) paneResult(ctx context.Context, method string, params any) (PaneInfo, error) {
	var result struct {
		Type string   `json:"type"`
		Pane PaneInfo `json:"pane"`
	}
	err := c.Call(ctx, method, params, &result)
	return result.Pane, err
}

func (c *Client) Agents(ctx context.Context) ([]AgentInfo, error) {
	var result struct {
		Type   string      `json:"type"`
		Agents []AgentInfo `json:"agents"`
	}
	err := c.Call(ctx, "agent.list", nil, &result)
	return result.Agents, err
}

func (c *Client) ExportLayout(ctx context.Context, params LayoutExportParams) (LayoutDescription, error) {
	return c.layoutResult(ctx, "layout.export", params)
}

func (c *Client) SetSplitRatio(ctx context.Context, params LayoutSetSplitRatioParams) (LayoutDescription, error) {
	return c.layoutResult(ctx, "layout.set_split_ratio", params)
}

func (c *Client) layoutResult(ctx context.Context, method string, params any) (LayoutDescription, error) {
	var result struct {
		Type   string            `json:"type"`
		Layout LayoutDescription `json:"layout"`
	}
	err := c.Call(ctx, method, params, &result)
	return result.Layout, err
}

func (c *Client) Subscribe(ctx context.Context, subscriptions ...EventSubscription) (*Subscription, error) {
	id := c.requestID()
	conn, err := c.connect(ctx)
	if err != nil {
		return nil, err
	}
	stop, err := applyContext(ctx, conn, c.Timeout)
	if err != nil {
		conn.Close()
		return nil, err
	}
	defer stop()

	if err := writeRequest(conn, request{
		ID:     id,
		Method: "events.subscribe",
		Params: map[string]any{"subscriptions": subscriptions},
	}); err != nil {
		conn.Close()
		return nil, err
	}
	reader := bufio.NewReader(conn)
	raw, err := readResponse(reader, id)
	if err != nil {
		conn.Close()
		return nil, err
	}
	var ack struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(raw, &ack); err != nil {
		conn.Close()
		return nil, fmt.Errorf("decode subscription response: %w", err)
	}
	if ack.Type != "subscription_started" {
		conn.Close()
		return nil, fmt.Errorf("unexpected Herdr result type %q", ack.Type)
	}
	if err := conn.SetDeadline(time.Time{}); err != nil {
		conn.Close()
		return nil, err
	}
	return &Subscription{conn: conn, reader: reader}, nil
}

type Subscription struct {
	conn   net.Conn
	reader *bufio.Reader
}

func (s *Subscription) Next(ctx context.Context) (EventEnvelope, error) {
	stop, err := applyContext(ctx, s.conn, 0)
	if err != nil {
		return EventEnvelope{}, err
	}
	defer stop()

	line, err := s.reader.ReadBytes('\n')
	if err != nil {
		return EventEnvelope{}, err
	}
	var event EventEnvelope
	if err := json.Unmarshal(line, &event); err != nil {
		return EventEnvelope{}, fmt.Errorf("decode Herdr event: %w", err)
	}
	return event, nil
}

func (s *Subscription) Close() error {
	return s.conn.Close()
}

func (c *Client) connect(ctx context.Context) (net.Conn, error) {
	if c.SocketPath == "" {
		return nil, ErrMissingSocketPath
	}
	conn, err := dialLocal(ctx, c.SocketPath)
	if err != nil {
		return nil, fmt.Errorf("connect to Herdr socket %q: %w", c.SocketPath, err)
	}
	return conn, nil
}

func (c *Client) requestID() string {
	return fmt.Sprintf("herdr-client:%d:%d", os.Getpid(), c.nextID.Add(1))
}

func writeRequest(writer io.Writer, value request) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("encode Herdr request: %w", err)
	}
	encoded = append(encoded, '\n')
	if _, err := writer.Write(encoded); err != nil {
		return fmt.Errorf("write Herdr request: %w", err)
	}
	return nil
}

func readResponse(reader *bufio.Reader, expectedID string) (json.RawMessage, error) {
	line, err := reader.ReadBytes('\n')
	if err != nil {
		return nil, fmt.Errorf("read Herdr response: %w", err)
	}
	var value response
	if err := json.Unmarshal(line, &value); err != nil {
		return nil, fmt.Errorf("decode Herdr response: %w", err)
	}
	if value.ID != expectedID {
		return nil, fmt.Errorf("Herdr response id %q does not match %q", value.ID, expectedID)
	}
	if value.Error != nil {
		return nil, value.Error
	}
	if len(value.Result) == 0 {
		return nil, errors.New("Herdr response has neither result nor error")
	}
	return value.Result, nil
}

func nonnilParams(params any) any {
	if params == nil {
		return map[string]any{}
	}
	return params
}

func applyContext(ctx context.Context, conn net.Conn, timeout time.Duration) (func() bool, error) {
	var deadline time.Time
	if contextDeadline, ok := ctx.Deadline(); ok {
		deadline = contextDeadline
	}
	if timeout > 0 {
		timeoutDeadline := time.Now().Add(timeout)
		if deadline.IsZero() || timeoutDeadline.Before(deadline) {
			deadline = timeoutDeadline
		}
	}
	if err := conn.SetDeadline(deadline); err != nil {
		return nil, err
	}
	stop := context.AfterFunc(ctx, func() {
		_ = conn.SetDeadline(time.Now())
	})
	return stop, nil
}
