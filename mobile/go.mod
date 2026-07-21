module ssh2socks.local/mobile

go 1.26.3

// The gomobile bind layer. Built ONLY in the Android toolchain environment
// (gomobile + NDK); it is intentionally a separate module so the pure `core`
// module stays offline-buildable and unit-testable.
require (
	github.com/xjasonlyu/tun2socks/v2 v2.7.0
	ssh2socks.local/core v0.0.0
)

replace ssh2socks.local/core => ../core
