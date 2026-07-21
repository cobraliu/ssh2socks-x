import 'package:device_apps/device_apps.dart';
import 'package:flutter/material.dart';

/// Multi-select of installed launchable apps for per-app VPN routing.
class AppPickerScreen extends StatefulWidget {
  final List<String> selected;
  const AppPickerScreen({super.key, required this.selected});
  @override
  State<AppPickerScreen> createState() => _AppPickerScreenState();
}

class _AppPickerScreenState extends State<AppPickerScreen> {
  List<Application> _apps = [];
  late Set<String> _sel;
  String _query = '';

  @override
  void initState() {
    super.initState();
    _sel = widget.selected.toSet();
    DeviceApps.getInstalledApplications(
      includeSystemApps: false,
      onlyAppsWithLaunchIntent: true,
    ).then((list) {
      list.sort((a, b) => a.appName.toLowerCase().compareTo(b.appName.toLowerCase()));
      setState(() => _apps = list);
    });
  }

  @override
  Widget build(BuildContext context) {
    final apps = _apps
        .where((a) => _query.isEmpty || a.appName.toLowerCase().contains(_query.toLowerCase()))
        .toList();
    return Scaffold(
      appBar: AppBar(
        title: Text('选择应用 (${_sel.length})'),
        actions: [
          IconButton(
            icon: const Icon(Icons.check),
            onPressed: () => Navigator.pop(context, _sel.toList()),
          ),
        ],
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(56),
          child: Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
            child: TextField(
              decoration: const InputDecoration(
                hintText: '搜索应用',
                prefixIcon: Icon(Icons.search),
                border: OutlineInputBorder(),
                isDense: true,
              ),
              onChanged: (v) => setState(() => _query = v),
            ),
          ),
        ),
      ),
      body: ListView.builder(
        itemCount: apps.length,
        itemBuilder: (_, i) {
          final a = apps[i];
          return CheckboxListTile(
            title: Text(a.appName),
            subtitle: Text(a.packageName, style: const TextStyle(fontSize: 11)),
            value: _sel.contains(a.packageName),
            onChanged: (v) => setState(() {
              if (v == true) {
                _sel.add(a.packageName);
              } else {
                _sel.remove(a.packageName);
              }
            }),
          );
        },
      ),
    );
  }
}
