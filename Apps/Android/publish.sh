#!/bin/sh
# Publishes a signed release APK to notify.mad-gamer.com, where the app's
# updater and the website's download link find it.
#
#   ./publish.sh path/to/app-release.apk "What's new in this release."
#
# Reads the version from the build's output-metadata.json next to the APK, uploads it as /downloads/mg-notify-<version>-<build>.apk,
# then writes /downloads/android.json (last, so apps never see a half-published release)
# and deletes every older APK.
set -e
APK="$1"
NOTES="${2:-}"
test -f "$APK" || { echo "usage: $0 app-release.apk [notes]"; exit 2; }
# The build writes the version next to the APK.
METADATA="$(dirname "$APK")/output-metadata.json"
test -f "$METADATA" || { echo "no output-metadata.json next to the APK"; exit 2; }
VERSION_NAME=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['elements'][0]['versionName'])" "$METADATA")
VERSION_CODE=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['elements'][0]['versionCode'])" "$METADATA")
# Each build gets its own file, so apps mid-download never see it change.
NAME="mg-notify-$VERSION_NAME-$VERSION_CODE.apk"
SHA256=$(shasum -a 256 "$APK" | cut -d' ' -f1)
SIZE=$(wc -c < "$APK" | tr -d ' ')
python3 - "$VERSION_NAME" "$VERSION_CODE" "$NAME" "$SHA256" "$SIZE" "$NOTES" > /tmp/android.json <<'PY'
import json, sys
name, code, file, sha, size, notes = sys.argv[1:]
print(json.dumps({
    "version_name": name,
    "version_code": int(code),
    "url": "https://notify.mad-gamer.com/downloads/" + file,
    "sha256": sha,
    "size": int(size),
    "notes": notes,
}, indent=2))
PY
scp -q "$APK" matthew@mad-gamer.com:/tmp/"$NAME"
scp -q /tmp/android.json matthew@mad-gamer.com:/tmp/android.json
rm /tmp/android.json
ssh matthew@mad-gamer.com "set -e
sudo mkdir -p /var/www/notify-downloads
sudo mv /tmp/$NAME /var/www/notify-downloads/$NAME
sudo ln -sfn $NAME /var/www/notify-downloads/mg-notify-latest.apk
sudo mv /tmp/android.json /var/www/notify-downloads/android.json
# Only the latest release is kept.
sudo find /var/www/notify-downloads -maxdepth 1 -type f -name 'mg-notify-*.apk' ! -name $NAME -delete
sudo chown -R www-data:www-data /var/www/notify-downloads
sudo chmod 644 /var/www/notify-downloads/*.apk /var/www/notify-downloads/android.json"
echo "Published MG Notify $VERSION_NAME (build $VERSION_CODE), sha256 $SHA256"
