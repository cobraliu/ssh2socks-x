import 'dart:convert';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';

import '../bridge.dart';
import '../models.dart';
import '../store.dart';
import 'app_picker_screen.dart';
import 'host_picker_screen.dart';

/// Create or edit a connection profile.
class EditScreen extends StatefulWidget {
  final Profile? profile;
  const EditScreen({super.key, this.profile});
  @override
  State<EditScreen> createState() => _EditScreenState();
}

class _EditScreenState extends State<EditScreen> {
  late Profile _p;
  final _name = TextEditingController();
  final _probe = TextEditingController();
  String _configText = '';
  String _keyPem = '';
  final _pass = TextEditingController();
  bool _isNew = false;

  @override
  void initState() {
    super.initState();
    _isNew = widget.profile == null;
    _p = widget.profile ??
        Profile(id: DateTime.now().microsecondsSinceEpoch.toString(), name: '');
    _name.text = _p.name;
    _probe.text = _p.probeUrl;
    _configText = _p.configText;
    if (!_isNew) {
      Store.getKey(_p.keyId).then((v) => _keyPem = v);
      Store.getPassphrase(_p.keyId).then((v) => _pass.text = v);
    }
  }

  Future<void> _importConfig() async {
    final res = await FilePicker.platform.pickFiles(withData: true);
    if (res == null) return;
    final bytes = res.files.single.bytes;
    if (bytes == null) return;
    setState(() => _configText = utf8.decode(bytes));
    if (!mounted) return;
    final host = await Navigator.push<HostInfo>(
      context,
      MaterialPageRoute(builder: (_) => HostPickerScreen(configText: _configText)),
    );
    if (host != null) {
      setState(() {
        _p.target = host.alias;
        if (_name.text.isEmpty) _name.text = host.alias;
      });
    }
  }

  Future<void> _importKey() async {
    final res = await FilePicker.platform.pickFiles(withData: true);
    if (res == null) return;
    final bytes = res.files.single.bytes;
    if (bytes == null) return;
    setState(() => _keyPem = utf8.decode(bytes));
    _snack('已导入私钥');
  }

  Future<void> _pickApps() async {
    final picked = await Navigator.push<List<String>>(
      context,
      MaterialPageRoute(
          builder: (_) => AppPickerScreen(selected: _p.allowedApps)),
    );
    if (picked != null) setState(() => _p.allowedApps = picked);
  }

  Future<void> _save() async {
    _p.name = _name.text.trim();
    _p.probeUrl = _probe.text.trim();
    _p.configText = _configText;
    if (_p.name.isEmpty || _p.target.isEmpty || _keyPem.isEmpty) {
      _snack('名称、目标主机、私钥均为必填');
      return;
    }
    await Store.putKey(_p.keyId, _keyPem);
    await Store.putPassphrase(_p.keyId, _pass.text);
    final list = await Store.loadProfiles();
    final i = list.indexWhere((e) => e.id == _p.id);
    if (i >= 0) {
      list[i] = _p;
    } else {
      list.add(_p);
    }
    await Store.saveProfiles(list);
    if (mounted) Navigator.pop(context, true);
  }

  Future<void> _delete() async {
    await Store.deleteKey(_p.keyId);
    final list = await Store.loadProfiles();
    list.removeWhere((e) => e.id == _p.id);
    await Store.saveProfiles(list);
    if (mounted) Navigator.pop(context, true);
  }

  void _snack(String m) =>
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(_isNew ? '新建连接' : '编辑连接'),
        actions: [
          if (!_isNew) IconButton(onPressed: _delete, icon: const Icon(Icons.delete_outline)),
          IconButton(onPressed: _save, icon: const Icon(Icons.check)),
        ],
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          TextField(controller: _name, decoration: const InputDecoration(labelText: '连接名称')),
          const SizedBox(height: 12),
          Card(
            child: ListTile(
              title: Text(_p.target.isEmpty ? '导入 OpenSSH config 并选择主机' : '目标: ${_p.target}'),
              subtitle: _configText.isEmpty ? null : const Text('已导入 config'),
              trailing: const Icon(Icons.file_open_outlined),
              onTap: _importConfig,
            ),
          ),
          Card(
            child: ListTile(
              title: Text(_keyPem.isEmpty ? '导入私钥 (PEM/OpenSSH)' : '私钥已就绪'),
              trailing: const Icon(Icons.key_outlined),
              onTap: _importKey,
            ),
          ),
          TextField(
            controller: _pass,
            obscureText: true,
            decoration: const InputDecoration(labelText: '私钥口令（可选）'),
          ),
          const SizedBox(height: 12),
          TextField(controller: _probe, decoration: const InputDecoration(labelText: '连通性探测 URL')),
          SwitchListTile(
            title: const Text('断线自动重连'),
            value: _p.autoReconnect,
            onChanged: (v) => setState(() => _p.autoReconnect = v),
          ),
          const Divider(),
          const Text('路由模式', style: TextStyle(fontWeight: FontWeight.bold)),
          RadioListTile<RoutingMode>(
            title: const Text('全局（所有应用走代理）'),
            value: RoutingMode.global,
            groupValue: _p.routingMode,
            onChanged: (v) => setState(() => _p.routingMode = v!),
          ),
          RadioListTile<RoutingMode>(
            title: const Text('分应用（仅所选应用走代理）'),
            value: RoutingMode.perApp,
            groupValue: _p.routingMode,
            onChanged: (v) => setState(() => _p.routingMode = v!),
          ),
          if (_p.routingMode == RoutingMode.perApp)
            ListTile(
              title: Text('已选 ${_p.allowedApps.length} 个应用'),
              trailing: const Icon(Icons.apps),
              onTap: _pickApps,
            ),
        ],
      ),
    );
  }
}
