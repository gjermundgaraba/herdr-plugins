package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestInvocationCWD(t *testing.T) {
	t.Parallel()

	cwd, err := invocationCWD(`{"focused_pane_cwd":"/focused","workspace_cwd":"/workspace"}`)
	if err != nil {
		t.Fatal(err)
	}
	if cwd != "/focused" {
		t.Fatalf("cwd = %q, want /focused", cwd)
	}

	cwd, err = invocationCWD(`{"workspace_cwd":"/workspace"}`)
	if err != nil {
		t.Fatal(err)
	}
	if cwd != "/workspace" {
		t.Fatalf("cwd = %q, want /workspace", cwd)
	}
}

func TestLoadShellSettingsPrefersConfig(t *testing.T) {
	t.Setenv("SHELL", "/bin/zsh")
	path := filepath.Join(t.TempDir(), "config.toml")
	if err := os.WriteFile(path, []byte("[terminal]\ndefault_shell = 'fish'\nshell_mode = 'non_login'\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	settings, err := loadShellSettings(path)
	if err != nil {
		t.Fatal(err)
	}
	if settings != (shellSettings{shell: "fish", mode: "non_login"}) {
		t.Fatalf("settings = %#v", settings)
	}
}

func TestShellCommandMatchesHerdrModes(t *testing.T) {
	t.Parallel()

	path, argv, login, err := shellCommand("/bin/sh", "auto", "darwin")
	if err != nil {
		t.Fatal(err)
	}
	if path != "/bin/sh" || !login || len(argv) != 1 || argv[0] != "-sh" {
		t.Fatalf("macOS auto = path %q, argv %#v, login %v", path, argv, login)
	}

	_, argv, login, err = shellCommand("/bin/sh", "auto", "linux")
	if err != nil {
		t.Fatal(err)
	}
	if login || len(argv) != 1 || argv[0] != "/bin/sh" {
		t.Fatalf("Linux auto = argv %#v, login %v", argv, login)
	}
}
