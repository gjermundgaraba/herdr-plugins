package main

import (
	"cmp"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"

	"github.com/BurntSushi/toml"
)

type invocationContext struct {
	FocusedPaneCWD string `json:"focused_pane_cwd"`
	WorkspaceCWD   string `json:"workspace_cwd"`
}

type herdrConfig struct {
	Terminal struct {
		DefaultShell string `toml:"default_shell"`
		ShellMode    string `toml:"shell_mode"`
	} `toml:"terminal"`
}

type shellSettings struct {
	shell string
	mode  string
}

func main() {
	if err := startShell(); err != nil {
		fmt.Fprintf(os.Stderr, "popup-terminal: %v\n", err)
		os.Exit(1)
	}
}

func startShell() error {
	cwd, err := invocationCWD(os.Getenv("HERDR_PLUGIN_CONTEXT_JSON"))
	if err != nil {
		return err
	}
	settings, err := loadShellSettings(configPath())
	if err != nil {
		return err
	}
	if err := os.Chdir(cwd); err != nil {
		return fmt.Errorf("change directory to %q: %w", cwd, err)
	}

	shellPath, argv, login, err := shellCommand(settings.shell, settings.mode, runtime.GOOS)
	if err != nil {
		return err
	}
	if login {
		if err := os.Setenv("SHELL", shellPath); err != nil {
			return fmt.Errorf("set SHELL: %w", err)
		}
	}
	return syscall.Exec(shellPath, argv, os.Environ())
}

func invocationCWD(raw string) (string, error) {
	var context invocationContext
	if err := json.Unmarshal([]byte(raw), &context); err != nil {
		return "", fmt.Errorf("parse HERDR_PLUGIN_CONTEXT_JSON: %w", err)
	}
	cwd := cmp.Or(context.FocusedPaneCWD, context.WorkspaceCWD)
	if cwd == "" {
		return "", errors.New("no focused pane or workspace working directory")
	}
	return cwd, nil
}

func configPath() string {
	if path := os.Getenv("HERDR_CONFIG_PATH"); path != "" {
		return path
	}
	base := os.Getenv("XDG_CONFIG_HOME")
	if base == "" {
		if home := os.Getenv("HOME"); home != "" {
			base = filepath.Join(home, ".config")
		}
	}
	if base == "" {
		return ""
	}
	return filepath.Join(base, "herdr", "config.toml")
}

func loadShellSettings(path string) (shellSettings, error) {
	var config herdrConfig
	if path != "" {
		if _, err := toml.DecodeFile(path, &config); err != nil && !os.IsNotExist(err) {
			return shellSettings{}, fmt.Errorf("read Herdr config: %w", err)
		}
	}

	return shellSettings{
		shell: cmp.Or(
			strings.TrimSpace(config.Terminal.DefaultShell),
			strings.TrimSpace(os.Getenv("SHELL")),
			"/bin/sh",
		),
		mode: cmp.Or(strings.TrimSpace(config.Terminal.ShellMode), "auto"),
	}, nil
}

func shellCommand(shell, mode, goos string) (string, []string, bool, error) {
	shellPath, err := exec.LookPath(shell)
	if err != nil {
		return "", nil, false, fmt.Errorf("find shell %q: %w", shell, err)
	}
	login, err := usesLoginShell(mode, goos)
	if err != nil {
		return "", nil, false, err
	}
	argv0 := shell
	if login {
		argv0 = "-" + filepath.Base(shellPath)
	}
	return shellPath, []string{argv0}, login, nil
}

func usesLoginShell(mode, goos string) (bool, error) {
	switch mode {
	case "auto":
		return goos == "darwin", nil
	case "login":
		return true, nil
	case "non_login":
		return false, nil
	default:
		return false, fmt.Errorf("invalid Herdr shell mode %q", mode)
	}
}
