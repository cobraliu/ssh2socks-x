import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'models.dart';

/// Profiles live in SharedPreferences (non-secret); private keys and
/// passphrases live in flutter_secure_storage (Android Keystore-backed).
class Store {
  static const _profilesKey = 'profiles';
  static const _secure = FlutterSecureStorage();

  static Future<List<Profile>> loadProfiles() async {
    final prefs = await SharedPreferences.getInstance();
    final raw = prefs.getString(_profilesKey);
    if (raw == null) return [];
    final list = jsonDecode(raw) as List;
    return list.map((e) => Profile.fromJson(e as Map<String, dynamic>)).toList();
  }

  static Future<void> saveProfiles(List<Profile> profiles) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_profilesKey, jsonEncode(profiles.map((p) => p.toJson()).toList()));
  }

  static Future<void> putKey(String keyId, String pem) =>
      _secure.write(key: 'key_$keyId', value: pem);

  static Future<String> getKey(String keyId) async =>
      await _secure.read(key: 'key_$keyId') ?? '';

  static Future<void> putPassphrase(String keyId, String pass) =>
      _secure.write(key: 'pass_$keyId', value: pass);

  static Future<String> getPassphrase(String keyId) async =>
      await _secure.read(key: 'pass_$keyId') ?? '';

  static Future<void> deleteKey(String keyId) async {
    await _secure.delete(key: 'key_$keyId');
    await _secure.delete(key: 'pass_$keyId');
  }
}
