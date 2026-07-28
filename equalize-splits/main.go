package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"syscall"
	"time"

	herdr "github.com/gjermundgaraba/herdr-plugins/sdk/go"
)

const (
	socketTimeout = time.Second
	// ponytail: Herdr layout.apply caps layouts at 24 panes; this also bounds
	// retries if another client continuously changes the same layout.
	maxRatioUpdates = 64
)

type ratioUpdate struct {
	Path  []bool
	Ratio float64
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "equalize-splits:", err)
		os.Exit(1)
	}
}

func run() error {
	environment, err := herdr.LoadEnvironment()
	if err != nil {
		return err
	}
	if environment.Event == nil {
		return nil
	}

	paneID, err := eventPaneID(environment.Event.Data)
	if err != nil {
		return err
	}
	if paneID == "" {
		return nil
	}

	lock, err := acquireLock(environment.PluginStateDir)
	if err != nil {
		return err
	}
	defer releaseLock(lock)

	client, err := herdr.NewFromEnv()
	if err != nil {
		return err
	}
	client.Timeout = socketTimeout

	layout, err := client.ExportLayout(
		context.Background(),
		herdr.LayoutExportParams{PaneID: paneID},
	)
	if err != nil {
		return fmt.Errorf("export layout: %w", err)
	}

	for range maxRatioUpdates {
		updates, err := equalizationPlan(layout.Root, paneID)
		if err != nil {
			return err
		}
		if len(updates) == 0 {
			return nil
		}
		update := updates[0]
		layout, err = client.SetSplitRatio(
			context.Background(),
			herdr.LayoutSetSplitRatioParams{
				TabID: layout.TabID,
				Path:  update.Path,
				Ratio: update.Ratio,
			},
		)
		if err != nil {
			return fmt.Errorf("set split ratio: %w", err)
		}
	}
	return errors.New("layout kept changing while equalizing")
}

func eventPaneID(data json.RawMessage) (string, error) {
	var event struct {
		Pane struct {
			PaneID string `json:"pane_id"`
		} `json:"pane"`
	}
	if err := json.Unmarshal(data, &event); err != nil {
		return "", fmt.Errorf("decode pane.created event: %w", err)
	}
	return event.Pane.PaneID, nil
}

func acquireLock(stateDir string) (*os.File, error) {
	if stateDir == "" {
		return nil, errors.New("HERDR_PLUGIN_STATE_DIR is not set")
	}
	lock, err := os.OpenFile(filepath.Join(stateDir, "equalize.lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open lock: %w", err)
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX); err != nil {
		lock.Close()
		return nil, fmt.Errorf("acquire lock: %w", err)
	}
	return lock, nil
}

func releaseLock(lock *os.File) {
	_ = syscall.Flock(int(lock.Fd()), syscall.LOCK_UN)
	_ = lock.Close()
}

func equalizationPlan(root *herdr.LayoutNode, paneID string) ([]ratioUpdate, error) {
	panePath, found, err := findPanePath(root, paneID, nil)
	if err != nil {
		return nil, err
	}
	if !found || len(panePath) == 0 {
		return nil, nil // Root pane creation is not a split.
	}
	if !panePath[len(panePath)-1] {
		return nil, nil // A newer split already moved this pane from its creation position.
	}

	regionPath := clonePath(panePath[:len(panePath)-1])
	parent, err := nodeAt(root, regionPath)
	if err != nil {
		return nil, err
	}
	if parent.Type != "split" || parent.Direction == "" {
		return nil, errors.New("invalid layout: pane parent is not a directional split")
	}
	direction := parent.Direction

	for len(regionPath) > 0 {
		ancestorPath := clonePath(regionPath[:len(regionPath)-1])
		ancestor, err := nodeAt(root, ancestorPath)
		if err != nil {
			return nil, err
		}
		if ancestor.Type != "split" || ancestor.Direction != direction {
			break
		}
		regionPath = ancestorPath
	}

	region, err := nodeAt(root, regionPath)
	if err != nil {
		return nil, err
	}
	var updates []ratioUpdate
	if _, err := collectEqualizations(region, direction, regionPath, &updates); err != nil {
		return nil, err
	}
	return updates, nil
}

func findPanePath(node *herdr.LayoutNode, paneID string, path []bool) ([]bool, bool, error) {
	if node == nil {
		return nil, false, errors.New("invalid layout: missing node")
	}
	switch node.Type {
	case "pane":
		return path, node.PaneID == paneID, nil
	case "split":
		if foundPath, found, err := findPanePath(node.First, paneID, appendPath(path, false)); err != nil || found {
			return foundPath, found, err
		}
		return findPanePath(node.Second, paneID, appendPath(path, true))
	default:
		return nil, false, fmt.Errorf("invalid layout node type %q", node.Type)
	}
}

func nodeAt(node *herdr.LayoutNode, path []bool) (*herdr.LayoutNode, error) {
	for _, second := range path {
		if node == nil || node.Type != "split" {
			return nil, errors.New("invalid layout: split path crosses a pane")
		}
		if second {
			node = node.Second
		} else {
			node = node.First
		}
	}
	if node == nil {
		return nil, errors.New("invalid layout: split path ends at a missing node")
	}
	return node, nil
}

func collectEqualizations(node *herdr.LayoutNode, direction string, path []bool, updates *[]ratioUpdate) (int, error) {
	if node == nil {
		return 0, errors.New("invalid layout: missing child")
	}
	switch node.Type {
	case "pane":
		return 1, nil
	case "split":
		if node.Direction != direction {
			return 1, nil
		}
	default:
		return 0, fmt.Errorf("invalid layout node type %q", node.Type)
	}

	first, err := collectEqualizations(node.First, direction, appendPath(path, false), updates)
	if err != nil {
		return 0, err
	}
	second, err := collectEqualizations(node.Second, direction, appendPath(path, true), updates)
	if err != nil {
		return 0, err
	}
	ratio := float64(first) / float64(first+second)
	ratio = math.Max(0.1, math.Min(0.9, ratio))
	if math.Abs(node.Ratio-ratio) > 1e-6 {
		*updates = append(*updates, ratioUpdate{clonePath(path), ratio})
	}
	return first + second, nil
}

func appendPath(path []bool, side bool) []bool {
	next := make([]bool, len(path)+1)
	copy(next, path)
	next[len(path)] = side
	return next
}

func clonePath(path []bool) []bool {
	cloned := make([]bool, len(path))
	copy(cloned, path)
	return cloned
}
