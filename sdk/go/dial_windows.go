//go:build windows

package herdr

import (
	"context"
	"net"
	"strings"

	"github.com/Microsoft/go-winio"
)

func dialLocal(ctx context.Context, socketPath string) (net.Conn, error) {
	if !strings.HasPrefix(socketPath, `\\.\pipe\`) {
		socketPath = `\\.\pipe\` + socketPath
	}
	return winio.DialPipeContext(ctx, socketPath)
}
