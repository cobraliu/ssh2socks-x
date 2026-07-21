import 'dart:async';

import 'package:flutter/material.dart';

import 'bridge.dart';
import 'models.dart';
import 'screens/edit_screen.dart';
import 'store.dart';

void main() => runApp(const App());

class App extends StatelessWidget {
  const App({super.key});
  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'ssh2socks',
        theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
        darkTheme: ThemeData(
            colorSchemeSeed: Colors.indigo, brightness: Brightness.dark, useMaterial3: true),
        home: const HomeScreen(),
      );
}

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key});
  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  List<Profile> _profiles = [];
  String _state = 'stopped';
  String _activeId = '';
  String _lastProbe = '';
  final List<String> _log = [];
  StreamSubscription? _sub;

  @override
  void initState() {
    super.initState();
    _load();
    _sub = Bridge.events().listen(_onEvent);
    Bridge.currentState().then((s) => setState(() => _state = s));
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    final p = await Store.loadProfiles();
    setState(() => _profiles = p);
  }

  void _onEvent(Map<String, dynamic> e) {
    setState(() {
      switch (e['type']) {
        case 'state':
          _state = e['state'] ?? _state;
          if (_state == 'stopped' || _state == 'error') _activeId = '';
          break;
        case 'probe':
          final ok = e['ok'] == true;
          _lastProbe = '${ok ? '通' : '断'} · ${e['latencyMs']}ms · ${e['message'] ?? ''}';
          break;
        case 'log':
          _log.insert(0, e['line'] ?? '');
          if (_log.length > 200) _log.removeLast();
          break;
      }
    });
  }

  Future<void> _connect(Profile p) async {
    if (!await Bridge.prepareVpn()) {
      _snack('未授予 VPN 权限');
      return;
    }
    final pem = await Store.getKey(p.keyId);
    if (pem.isEmpty) {
      _snack('该连接未导入私钥');
      return;
    }
    final pass = await Store.getPassphrase(p.keyId);
    await Bridge.start(p.toEngineJson(privateKeyPem: pem, passphrase: pass));
    setState(() => _activeId = p.id);
  }

  Future<void> _disconnect() async {
    await Bridge.stop();
    setState(() => _activeId = '');
  }

  void _snack(String m) =>
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));

  Future<void> _edit([Profile? p]) async {
    final saved = await Navigator.push<bool>(
      context,
      MaterialPageRoute(builder: (_) => EditScreen(profile: p)),
    );
    if (saved == true) _load();
  }

  Color _stateColor() => switch (_state) {
        'connected' => Colors.green,
        'connecting' => Colors.orange,
        'error' => Colors.red,
        _ => Colors.grey,
      };

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('ssh2socks'),
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(28),
          child: Padding(
            padding: const EdgeInsets.only(bottom: 6, left: 16, right: 16),
            child: Row(children: [
              Icon(Icons.circle, size: 12, color: _stateColor()),
              const SizedBox(width: 8),
              Text(_state),
              const Spacer(),
              if (_lastProbe.isNotEmpty)
                Text(_lastProbe, style: Theme.of(context).textTheme.bodySmall),
            ]),
          ),
        ),
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: () => _edit(),
        child: const Icon(Icons.add),
      ),
      body: _profiles.isEmpty
          ? const Center(child: Text('点击 + 新建连接'))
          : ListView(
              children: [
                ..._profiles.map(_tile),
                if (_log.isNotEmpty) const Divider(),
                if (_log.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.all(12),
                    child: Text(_log.take(12).join('\n'),
                        style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
                  ),
              ],
            ),
    );
  }

  Widget _tile(Profile p) {
    final active = p.id == _activeId;
    return ListTile(
      leading: Icon(active ? Icons.vpn_lock : Icons.dns_outlined,
          color: active ? _stateColor() : null),
      title: Text(p.name),
      subtitle: Text(p.configText.isNotEmpty
          ? '${p.target} · ${p.routingMode == RoutingMode.global ? '全局' : '分应用'}'
          : '${p.user}@${p.host}:${p.port}'),
      trailing: active
          ? FilledButton.tonal(onPressed: _disconnect, child: const Text('断开'))
          : FilledButton(
              onPressed: _activeId.isEmpty ? () => _connect(p) : null,
              child: const Text('连接')),
      onLongPress: () => _edit(p),
    );
  }
}
