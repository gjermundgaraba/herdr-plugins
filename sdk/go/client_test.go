//go:build !windows

package herdr

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPingRoundTripsOverNDJSON(t *testing.T) {
	path := testSocketPath(t, "ping")
	listener, err := net.Listen("unix", path)
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()

	done := make(chan error, 1)
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			done <- err
			return
		}
		defer conn.Close()
		line, err := bufio.NewReader(conn).ReadBytes('\n')
		if err != nil {
			done <- err
			return
		}
		var request request
		if err := json.Unmarshal(line, &request); err != nil {
			done <- err
			return
		}
		if request.Method != "ping" {
			done <- errors.New("expected ping request")
			return
		}
		_, err = conn.Write([]byte(`{"id":"` + request.ID + `","result":{"type":"pong","version":"0.7.5","protocol":17}}` + "\n"))
		done <- err
	}()

	ping, err := New(path).Ping(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if ping.Version != "0.7.5" || ping.Protocol != 17 {
		t.Fatalf("unexpected ping: %#v", ping)
	}
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func TestAPIErrorKeepsServerCode(t *testing.T) {
	line := `{"id":"x","error":{"code":"not_found","message":"pane not found"}}` + "\n"
	result, err := readResponse(bufio.NewReader(strings.NewReader(line)), "x")
	if result != nil {
		t.Fatalf("unexpected result: %s", result)
	}
	var apiError *APIError
	if !errors.As(err, &apiError) || apiError.Code != "not_found" {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestSubscriptionKeepsReadingAfterAck(t *testing.T) {
	path := testSocketPath(t, "subscription")
	listener, err := net.Listen("unix", path)
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()

	done := make(chan error, 1)
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			done <- err
			return
		}
		defer conn.Close()
		line, err := bufio.NewReader(conn).ReadBytes('\n')
		if err != nil {
			done <- err
			return
		}
		var request request
		if err := json.Unmarshal(line, &request); err != nil {
			done <- err
			return
		}
		if request.Method != "events.subscribe" {
			done <- errors.New("expected events.subscribe request")
			return
		}
		if _, err := conn.Write([]byte(`{"id":"` + request.ID + `","result":{"type":"subscription_started"}}` + "\n")); err != nil {
			done <- err
			return
		}
		_, err = conn.Write([]byte(`{"event":"pane.focused","data":{"type":"pane_focused","pane_id":"w1:p1"}}` + "\n"))
		done <- err
	}()

	subscription, err := New(path).Subscribe(
		context.Background(),
		NewEventSubscription("pane.focused"),
	)
	if err != nil {
		t.Fatal(err)
	}
	defer subscription.Close()
	event, err := subscription.Next(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if event.Event != "pane.focused" {
		t.Fatalf("unexpected event: %#v", event)
	}
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func testSocketPath(t *testing.T, name string) string {
	t.Helper()
	path := filepath.Join(os.TempDir(), fmt.Sprintf("herdr-go-%d-%s.sock", os.Getpid(), name))
	_ = os.Remove(path)
	t.Cleanup(func() { _ = os.Remove(path) })
	return path
}
