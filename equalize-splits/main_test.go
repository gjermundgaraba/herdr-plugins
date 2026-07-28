package main

import (
	"encoding/json"
	"reflect"
	"testing"

	herdr "github.com/gjermundgaraba/herdr-plugins/sdk/go"
)

func TestEqualizesThreeSideBySidePanes(t *testing.T) {
	root := split("right", pane("a"), splitWithRatio("right", pane("b"), pane("c"), 0.4))

	got, err := equalizationPlan(root, "c")
	if err != nil {
		t.Fatal(err)
	}
	want := []ratioUpdate{
		{Path: []bool{true}, Ratio: 1.0 / 2.0},
		{Path: []bool{}, Ratio: 1.0 / 3.0},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("updates = %#v, want %#v", got, want)
	}
}

func TestDoesNotEqualizeSameDirectionSplitAcrossOrthogonalBoundary(t *testing.T) {
	root := split(
		"down",
		splitWithRatio("right", pane("a"), pane("b"), 0.7),
		splitWithRatio("right", pane("c"), pane("d"), 0.8),
	)

	got, err := equalizationPlan(root, "d")
	if err != nil {
		t.Fatal(err)
	}
	want := []ratioUpdate{{Path: []bool{true}, Ratio: 0.5}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("updates = %#v, want %#v", got, want)
	}
}

func TestRootPaneCreationHasNoSplitDirection(t *testing.T) {
	root := pane("a")
	got, err := equalizationPlan(root, "a")
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 0 {
		t.Fatalf("updates = %#v, want none", got)
	}
}

func TestSkipsStaleCreationEventAfterPaneWasSplitAgain(t *testing.T) {
	root := split("right", pane("a"), split("down", pane("b"), pane("c")))

	got, err := equalizationPlan(root, "b")
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 0 {
		t.Fatalf("updates = %#v, want none", got)
	}
}

func TestHerdrWireFixtures(t *testing.T) {
	var response struct {
		Layout herdr.LayoutDescription `json:"layout"`
	}
	if err := json.Unmarshal([]byte(`{
		"layout": {
			"tab_id": "w1:t1",
			"root": {
				"type": "split",
				"direction": "right",
				"ratio": 0.5,
				"first": {"type": "pane", "pane_id": "w1:p1"},
				"second": {
					"type": "split",
					"direction": "right",
					"ratio": 0.5,
					"first": {"type": "pane", "pane_id": "w1:p2"},
					"second": {"type": "pane", "pane_id": "w1:p3"}
				}
			}
		}
	}`), &response); err != nil {
		t.Fatal(err)
	}
	if response.Layout.TabID != "w1:t1" {
		t.Fatalf("tab id = %q", response.Layout.TabID)
	}
	updates, err := equalizationPlan(response.Layout.Root, "w1:p3")
	if err != nil {
		t.Fatal(err)
	}
	if len(updates) != 1 || !reflect.DeepEqual(updates[0].Path, []bool{}) {
		t.Fatalf("updates = %#v, want root update", updates)
	}

	paneID, err := eventPaneID(json.RawMessage(`{"pane":{"pane_id":"w1:p3"}}`))
	if err != nil {
		t.Fatal(err)
	}
	if paneID != "w1:p3" {
		t.Fatalf("pane id = %q", paneID)
	}
}

func pane(id string) *herdr.LayoutNode {
	return &herdr.LayoutNode{Type: "pane", PaneID: id}
}

func split(direction string, first, second *herdr.LayoutNode) *herdr.LayoutNode {
	return splitWithRatio(direction, first, second, 0.5)
}

func splitWithRatio(direction string, first, second *herdr.LayoutNode, ratio float64) *herdr.LayoutNode {
	return &herdr.LayoutNode{
		Type:      "split",
		Direction: direction,
		Ratio:     ratio,
		First:     first,
		Second:    second,
	}
}
