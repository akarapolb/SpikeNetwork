# R8 rules for the release build.
# The Rust library (libspike_mobile.so) finds its Java side by name: the class and its native methods must
# keep their names (JNI looks up Java_network_spike_connect_Native_open/call/close).
-keep class network.spike.connect.Native { *; }
-keepclasseswithmembernames class * { native <methods>; }
# Readable stack traces for crash reports from release builds (keep the mapping file that R8 writes with each release).
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
# Drop verbose/debug/info logging in release; warnings and errors remain.
-assumenosideeffects class android.util.Log {
    public static int v(...);
    public static int d(...);
    public static int i(...);
}

# WebRTC (voice calls) is reached from native code by name.
-keep class org.webrtc.** { *; }
-dontwarn org.webrtc.**
