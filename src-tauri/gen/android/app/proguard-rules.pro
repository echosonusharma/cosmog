# Keep classes called from Rust via JNI. R8 cannot see these call sites
# and would strip or rename the methods without these rules.
-keep class com.sonus.cosmog.SecretStore { *; }
-keep class com.sonus.cosmog.TransferService { *; }

# Tink (used by EncryptedSharedPreferences) references javax.annotation at
# compile time only; suppress R8 warnings about missing annotation classes.
-dontwarn javax.annotation.Nullable
-dontwarn javax.annotation.concurrent.GuardedBy

# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile