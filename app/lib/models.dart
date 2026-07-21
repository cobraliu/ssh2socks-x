import 'dart:convert';

enum RoutingMode { global, perApp }

/// One saved connection profile.
class Profile {
  String id;
  String name;

  /// Either an imported OpenSSH config + target alias…
  String configText;
  String target;

  /// …or a direct host (used when configText is empty).
  String host;
  String port;
  String user;

  /// Key material is stored in secure storage; here we keep only its handle.
  String keyId;

  String probeUrl;
  bool autoReconnect;

  RoutingMode routingMode;
  List<String> allowedApps; // package names, used when routingMode == perApp

  Profile({
    required this.id,
    required this.name,
    this.configText = '',
    this.target = '',
    this.host = '',
    this.port = '22',
    this.user = '',
    this.keyId = '',
    this.probeUrl = 'http://www.gstatic.com/generate_204',
    this.autoReconnect = true,
    this.routingMode = RoutingMode.global,
    List<String>? allowedApps,
  }) : allowedApps = allowedApps ?? [];

  Map<String, dynamic> toJson() => {
        'id': id,
        'name': name,
        'configText': configText,
        'target': target,
        'host': host,
        'port': port,
        'user': user,
        'keyId': keyId,
        'probeUrl': probeUrl,
        'autoReconnect': autoReconnect,
        'routingMode': routingMode.name,
        'allowedApps': allowedApps,
      };

  factory Profile.fromJson(Map<String, dynamic> j) => Profile(
        id: j['id'],
        name: j['name'] ?? '',
        configText: j['configText'] ?? '',
        target: j['target'] ?? '',
        host: j['host'] ?? '',
        port: j['port'] ?? '22',
        user: j['user'] ?? '',
        keyId: j['keyId'] ?? '',
        probeUrl: j['probeUrl'] ?? 'http://www.gstatic.com/generate_204',
        autoReconnect: j['autoReconnect'] ?? true,
        routingMode: RoutingMode.values
            .firstWhere((m) => m.name == j['routingMode'], orElse: () => RoutingMode.global),
        allowedApps: (j['allowedApps'] as List?)?.cast<String>() ?? [],
      );

  /// Payload handed to the native service → Go core. [privateKeyPem] and
  /// [passphrase] are injected at connect time from secure storage.
  String toEngineJson({required String privateKeyPem, required String passphrase}) =>
      jsonEncode({
        'configText': configText,
        'target': target,
        'host': host,
        'port': port,
        'user': user,
        'privateKeyPem': privateKeyPem,
        'passphrase': passphrase,
        'defaultUser': user,
        'listenAddr': '127.0.0.1:0', // ephemeral; tun2socks reads SocksAddr()
        'probeUrl': probeUrl,
        'autoReconnect': autoReconnect,
        // VPN-level fields consumed by the Kotlin service (ignored by Go):
        'routingMode': routingMode.name,
        'allowedApps': allowedApps,
        'mtu': 1500,
        'dns': '1.1.1.1',
      });
}

/// A host parsed out of an imported OpenSSH config (mirrors Go core.HostInfo).
class HostInfo {
  final String alias;
  final String hostName;
  final String user;
  final String port;
  final String proxyChain;

  HostInfo(this.alias, this.hostName, this.user, this.port, this.proxyChain);

  factory HostInfo.fromJson(Map<String, dynamic> j) => HostInfo(
        j['alias'] ?? '',
        j['hostName'] ?? '',
        j['user'] ?? '',
        j['port'] ?? '',
        j['proxyChain'] ?? '',
      );

  bool matches(String q) {
    final s = q.toLowerCase();
    return alias.toLowerCase().contains(s) || hostName.toLowerCase().contains(s);
  }
}

class EngineStatus {
  final String state; // stopped | connecting | connected | error
  final String message;
  const EngineStatus(this.state, this.message);
}
