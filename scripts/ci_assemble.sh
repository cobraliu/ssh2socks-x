#!/usr/bin/env bash
# Assemble a buildable Flutter host project from our committed sources.
# Run from the repo root (CI does this before `flutter build apk`).
#
#   android/scripts/ci_assemble.sh
#
# It: (1) generates the Flutter/Gradle boilerplate we intentionally don't commit,
# (2) overlays our lib/ (kept), Kotlin, and AndroidManifest, (3) patches the
# app Gradle file for namespace/minSdk and the gomobile .aar dependency.
set -euo pipefail

proj="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # repo root (the android/ dir)
app="$proj/app"
pkg="com.ssh2socks.app"

echo "==> Backing up committed sources"
tmp="$(mktemp -d)"
cp -r "$app/lib" "$tmp/lib"
cp "$app/pubspec.yaml" "$tmp/pubspec.yaml"
cp -r "$app/android/app/src/main/kotlin" "$tmp/kotlin"
cp "$app/android/app/src/main/AndroidManifest.xml" "$tmp/AndroidManifest.xml"

echo "==> flutter create (fills gradle wrapper / settings / boilerplate)"
flutter create --org com.ssh2socks --project-name app --platforms=android "$app"

echo "==> Restoring our sources over the generated project"
rm -rf "$app/lib"; cp -r "$tmp/lib" "$app/lib"
cp "$tmp/pubspec.yaml" "$app/pubspec.yaml"
rm -rf "$app/android/app/src/main/kotlin"
cp -r "$tmp/kotlin" "$app/android/app/src/main/kotlin"
cp "$tmp/AndroidManifest.xml" "$app/android/app/src/main/AndroidManifest.xml"

echo "==> Patching app Gradle for package / minSdk / .aar dependency"
python3 - "$app" "$pkg" <<'PY'
import os, re, sys
app, pkg = sys.argv[1], sys.argv[2]
gdir = os.path.join(app, "android", "app")
path = os.path.join(gdir, "build.gradle")
kts = os.path.join(gdir, "build.gradle.kts")
if os.path.exists(kts):
    path = kts
src = open(path).read()
is_kts = path.endswith(".kts")

# namespace + applicationId -> our package
src = re.sub(r'namespace\s*=?\s*"[^"]*"', f'namespace = "{pkg}"' if is_kts else f'namespace "{pkg}"', src)
src = re.sub(r'applicationId\s*=?\s*"[^"]*"', f'applicationId = "{pkg}"' if is_kts else f'applicationId "{pkg}"', src)

# minSdk -> 24 (handles `minSdk = flutter.minSdkVersion`, `minSdkVersion 21`, `minSdk 21`)
src = re.sub(r'(minSdk(?:Version)?\s*=?\s*)[^\n]+', (r'\g<1>24' if is_kts else r'\g<1>24'), src, count=1)

# add the gomobile .aar dependency
dep = 'implementation(files("libs/mobile.aar"))' if is_kts else "implementation files('libs/mobile.aar')"
if "mobile.aar" not in src:
    m = re.search(r'\ndependencies\s*\{', src)
    if m:
        i = m.end()
        src = src[:i] + f"\n    {dep}" + src[i:]
    else:
        src += f"\n\ndependencies {{\n    {dep}\n}}\n"

# release signing (Groovy template). Reads key.properties if present (written
# in CI from GitHub secrets); otherwise the release build stays debug-signed so
# local builds without a keystore still work.
if not is_kts and "keystorePropertiesFile" not in src:
    load_block = (
        "def keystorePropertiesFile = rootProject.file('key.properties')\n"
        "def keystoreProperties = new Properties()\n"
        "if (keystorePropertiesFile.exists()) { "
        "keystoreProperties.load(new FileInputStream(keystorePropertiesFile)) }\n\n"
    )
    m = re.search(r'\nandroid\s*\{', src)
    if m:
        i = m.start() + 1  # start of the `android {` line (after plugins{} block)
        src = src[:i] + load_block + src[i:]

    sign_block = (
        "\n    signingConfigs {\n"
        "        release {\n"
        "            if (keystorePropertiesFile.exists()) {\n"
        "                keyAlias keystoreProperties['keyAlias']\n"
        "                keyPassword keystoreProperties['keyPassword']\n"
        "                storeFile keystoreProperties['storeFile'] ? file(keystoreProperties['storeFile']) : null\n"
        "                storePassword keystoreProperties['storePassword']\n"
        "            }\n"
        "        }\n"
        "    }\n"
    )
    m = re.search(r'\nandroid\s*\{', src)
    if m:
        i = m.end()
        src = src[:i] + sign_block + src[i:]

    # Flutter 3.24.5's template writes `signingConfig = signingConfigs.debug`
    # (with `=`); older templates omit it. Match either form.
    src = re.sub(
        r'signingConfig\s*=?\s*signingConfigs\.debug',
        "signingConfig = keystorePropertiesFile.exists() ? signingConfigs.release : signingConfigs.debug",
        src,
    )

open(path, "w").write(src)
print(f"patched {os.path.relpath(path, app)} (kts={is_kts})")
PY

echo "==> Injecting namespace fallback for legacy plugins (AGP 8 requires it)"
# Old plugins (e.g. device_apps) declare no `namespace` and relied on the
# manifest `package` attr, which AGP 8 ignores -> "Namespace not specified".
# Back-fill each library subproject's namespace from its manifest package.
python3 - "$app" <<'PY'
import os, sys
app = sys.argv[1]
groovy = os.path.join(app, "android", "build.gradle")
kts = os.path.join(app, "android", "build.gradle.kts")

groovy_block = r'''

// --- injected by ci_assemble.sh: namespace fallback for legacy plugins ---
subprojects {
    afterEvaluate { proj ->
        if (proj.extensions.findByName("android") != null) {
            def ext = proj.extensions.getByName("android")
            try {
                if (ext.namespace == null) {
                    def mf = new File(proj.projectDir, "src/main/AndroidManifest.xml")
                    if (mf.exists()) {
                        def m = (mf.text =~ /package="(.+?)"/)
                        if (m) { ext.namespace = m[0][1] }
                    }
                }
                // Legacy plugins pin an old compileSdk (<31); their transitive
                // AndroidX deps reference android:attr/lStar (API 31) -> AAPT
                // "resource lStar not found". Force a modern compileSdk.
                ext.compileSdkVersion 34
            } catch (ignored) {}
        }
    }
}
'''

kts_block = r'''

// --- injected by ci_assemble.sh: namespace fallback for legacy plugins ---
subprojects {
    afterEvaluate {
        val ext = extensions.findByName("android")
        if (ext is com.android.build.gradle.BaseExtension) {
            if (ext.namespace == null) {
                val mf = file("src/main/AndroidManifest.xml")
                if (mf.exists()) {
                    Regex("package=\"(.+?)\"").find(mf.readText())?.let {
                        ext.namespace = it.groupValues[1]
                    }
                }
            }
            ext.compileSdkVersion(34)
        }
    }
}
'''

def inject(path, block):
    # Insert BEFORE Flutter's own `subprojects { evaluationDependsOn(':app') }`
    # so our afterEvaluate hook is registered before those blocks force early
    # evaluation (otherwise: "Cannot run afterEvaluate when already evaluated").
    src = open(path).read()
    i = src.find("subprojects {")
    if i != -1:
        src = src[:i] + block.strip() + "\n\n" + src[i:]
    else:
        src = src + block
    open(path, "w").write(src)

if os.path.exists(groovy):
    inject(groovy, groovy_block)
    print("injected namespace fallback into android/build.gradle")
elif os.path.exists(kts):
    inject(kts, kts_block)
    print("injected namespace fallback into android/build.gradle.kts")
else:
    print("WARNING: no root gradle file found for namespace fallback")
PY

if [ -n "${KEYSTORE_BASE64:-}" ]; then
  echo "==> Materializing release keystore + key.properties from env"
  printf '%s' "$KEYSTORE_BASE64" | base64 -d > "$app/android/app/release.keystore"
  cat > "$app/android/key.properties" <<EOF
storeFile=release.keystore
storePassword=${KEYSTORE_PASSWORD:-}
keyAlias=${KEY_ALIAS:-}
keyPassword=${KEY_PASSWORD:-}
EOF
  echo "    wrote app/android/key.properties (release build will be release-signed)"
else
  echo "==> KEYSTORE_BASE64 not set; release build falls back to debug signing"
fi

echo "==> Assembly complete: $app"
