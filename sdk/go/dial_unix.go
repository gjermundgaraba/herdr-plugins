//go:build !windows

package herdr

import (
	"context"
	"net"
)

func dialLocal(ctx context.Context, socketPath string) (net.Conn, error) {
	return (&net.Dialer{}).DialContext(ctx, "unix", socketPath)
}
