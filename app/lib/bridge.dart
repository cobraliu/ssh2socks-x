import 'dart:async';
import 'dart:convert';

import 'package:flutter/services.dart';

import 'models.dart';

/// Thin wrapper over the platform channels exposed by MainActivity.
class Bridge {
  static const _method = MethodChannel('ssh2socks/vpn');
  static const _events = EventChannel('ssh2socks/events');

  /// Broadcast stream of engine events (state / log / probe).
  static Stream<Map<String, dynamic>> events() => _events
      .receiveBroadcastStream()
      .map((e) => jsonDecode(e as String) as Map<String, dynamic>);

  /// Ensures VPN consent has been granted (shows the system dialog if needed).
  /// Returns true once permission is available.
  static Future<bool> prepareVpn() async =>
      await _method.invokeMethod<bool>('prepareVpn') ?? false;

  /// Starts the VpnService with the given engine payload.
  static Future<void> start(String engineJson) =>
      _method.invokeMethod('start', {'config': engineJson});

  static Future<void> stop() => _method.invokeMethod('stop');

  static Future<String> currentState() async =>
      await _method.invokeMethod<String>('state') ?? 'stopped';

  /// Parses an OpenSSH config via the Go core, returning selectable hosts.
  static Future<List<HostInfo>> listHosts(String configText) async {
    final raw = await _method.invokeMethod<String>('listHosts', {'configText': configText});
    if (raw == null || raw.isEmpty) return [];
    final list = jsonDecode(raw) as List;
    return list.map((e) => HostInfo.fromJson(e as Map<String, dynamic>)).toList();
  }
}
