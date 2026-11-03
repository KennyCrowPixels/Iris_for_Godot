// Efficient on-device image handling: pick photos, then resize + JPEG-compress on the phone before
// sending. This is the key bandwidth saver — the desktop receives small base64 payloads, never the
// multi-megabyte original, and no raw camera data is streamed over Wi-Fi.

import * as ImageManipulator from "expo-image-manipulator";
import * as ImagePicker from "expo-image-picker";

const MAX_WIDTH = 1280;
const JPEG_QUALITY = 0.6;

async function compress(uri: string): Promise<string | null> {
  const result = await ImageManipulator.manipulateAsync(
    uri,
    [{ resize: { width: MAX_WIDTH } }],
    { compress: JPEG_QUALITY, format: ImageManipulator.SaveFormat.JPEG, base64: true },
  );
  return result.base64 ? `data:image/jpeg;base64,${result.base64}` : null;
}

export async function pickCompressedImages(limit = 4): Promise<string[]> {
  const permission = await ImagePicker.requestMediaLibraryPermissionsAsync();
  if (!permission.granted) throw new Error("Photo library permission was denied.");
  const result = await ImagePicker.launchImageLibraryAsync({
    mediaTypes: ImagePicker.MediaTypeOptions.Images,
    allowsMultipleSelection: true,
    selectionLimit: limit,
    quality: 1,
  });
  if (result.canceled) return [];
  const images: string[] = [];
  for (const asset of result.assets.slice(0, limit)) {
    const encoded = await compress(asset.uri);
    if (encoded) images.push(encoded);
  }
  return images;
}

export async function captureCompressedImage(): Promise<string[]> {
  const permission = await ImagePicker.requestCameraPermissionsAsync();
  if (!permission.granted) throw new Error("Camera permission was denied.");
  const result = await ImagePicker.launchCameraAsync({ quality: 1 });
  if (result.canceled || !result.assets[0]) return [];
  const encoded = await compress(result.assets[0].uri);
  return encoded ? [encoded] : [];
}
