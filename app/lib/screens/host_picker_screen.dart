import 'package:flutter/material.dart';

import '../bridge.dart';
import '../models.dart';

/// Picks a host from an imported OpenSSH config, searchable by alias or IP.
/// Hosts reached via a jump chain show the resolved chain as a subtitle.
class HostPickerScreen extends StatefulWidget {
  final String configText;
  const HostPickerScreen({super.key, required this.configText});
  @override
  State<HostPickerScreen> createState() => _HostPickerScreenState();
}

class _HostPickerScreenState extends State<HostPickerScreen> {
  List<HostInfo> _all = [];
  String _query = '';
  String? _error;

  @override
  void initState() {
    super.initState();
    Bridge.listHosts(widget.configText).then((h) {
      setState(() => _all = h);
    }).catchError((e) {
      setState(() => _error = '$e');
    });
  }

  @override
  Widget build(BuildContext context) {
    final hosts = _all.where((h) => _query.isEmpty || h.matches(_query)).toList();
    return Scaffold(
      appBar: AppBar(
        title: const Text('选择主机'),
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(56),
          child: Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
            child: TextField(
              autofocus: true,
              decoration: const InputDecoration(
                hintText: '按名称或 IP 搜索',
                prefixIcon: Icon(Icons.search),
                border: OutlineInputBorder(),
                isDense: true,
              ),
              onChanged: (v) => setState(() => _query = v),
            ),
          ),
        ),
      ),
      body: _error != null
          ? Center(child: Padding(padding: const EdgeInsets.all(24), child: Text('解析失败: $_error')))
          : ListView.builder(
              itemCount: hosts.length,
              itemBuilder: (_, i) {
                final h = hosts[i];
                return ListTile(
                  title: Text(h.alias),
                  subtitle: Text(h.proxyChain.isNotEmpty
                      ? '${h.hostName}  ·  链: ${h.proxyChain}'
                      : h.hostName),
                  onTap: () => Navigator.pop(context, h),
                );
              },
            ),
    );
  }
}
