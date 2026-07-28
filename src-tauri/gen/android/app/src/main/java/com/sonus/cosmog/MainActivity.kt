package com.sonus.cosmog

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat

class MainActivity : TauriActivity() {
  // Disable WryActivity's default back handling (WebView.canGoBack()/exit).
  // This SPA drives navigation from JS signal state, not WebView history, so
  // canGoBack() is always false and the OS back action would just exit.
  override val handleBackNavigation = false

  // ACTION_OPEN_DOCUMENT_TREE launcher for Night Watcher. Must be registered
  // before RESUMED, so it lives here and is fired via launchNwTreePicker().
  private lateinit var nwTreeLauncher: ActivityResultLauncher<Uri?>

  // POST_NOTIFICATIONS runtime prompt (A13+). Registered before RESUMED.
  private lateinit var notifPermLauncher: ActivityResultLauncher<String>

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    NativeBridge.initNdkContext(applicationContext)
    // Cache NW app-class GlobalRefs on this JVM thread (bug #3). Idempotent, so
    // harmless if CosmogApp.onCreate already did it. Guard so a native-init
    // failure never blocks the activity from starting.
    try {
      (application as? CosmogApp)?.initNwClasses()
    } catch (t: Throwable) {
      android.util.Log.w("MainActivity", "initNwClasses failed: $t")
    }
    nwTreeLauncher = registerForActivityResult(
      ActivityResultContracts.OpenDocumentTree()
    ) { uri ->
      NwTreePicker.onResult(uri)
    }
    // Register the permission launcher before RESUMED; result is advisory (the
    // FGS still starts, the notification is just suppressed if denied).
    notifPermLauncher = registerForActivityResult(
      ActivityResultContracts.RequestPermission()
    ) { /* granted-or-not: nothing to do, service handles absence gracefully */ }
    // On A13+ the foreground-service notification needs a runtime grant, else
    // it is silently suppressed. Request it now (an activity is the only place
    // a runtime prompt can surface).
    maybeRequestNotificationPermission()
    NwTreePicker.activity = this
    super.onCreate(savedInstanceState)
  }

  private fun maybeRequestNotificationPermission() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    val granted = ContextCompat.checkSelfPermission(
      this, Manifest.permission.POST_NOTIFICATIONS,
    ) == PackageManager.PERMISSION_GRANTED
    if (!granted) {
      try {
        notifPermLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
      } catch (t: Throwable) {
        android.util.Log.w("MainActivity", "notif perm request failed: $t")
      }
    }
  }

  override fun onResume() {
    super.onResume()
    // Consume any boot-deferred Night Watcher resume: BootReceiver cannot start
    // the dataSync FGS at boot on A12+, so it defers to the first foreground
    // moment, which is here. No-op if nothing is pending.
    try {
      NightWatchService.resumeIfPending(this)
    } catch (t: Throwable) {
      android.util.Log.w("MainActivity", "resumeIfPending failed: $t")
    }
  }

  // Invoked by NwTreePicker.launch() on the UI thread.
  fun launchNwTreePicker() {
    nwTreeLauncher.launch(null)
  }

  override fun onDestroy() {
    if (NwTreePicker.activity === this) {
      NwTreePicker.activity = null
    }
    super.onDestroy()
  }

  // Forward the Android back button / back gesture (gesture nav + 3-button)
  // into the web layer. window.__androidBack() returns "true" when the app
  // consumed the press (closed an overlay or stepped up a level); otherwise
  // fall through to the OS so the app backgrounds/exits from the top level.
  override fun onWebViewCreate(webView: WebView) {
    val cb = object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        webView.evaluateJavascript(
          "window.__androidBack ? window.__androidBack() : false"
        ) { result ->
          if (result != "true") {
            isEnabled = false
            onBackPressedDispatcher.onBackPressed()
            isEnabled = true
          }
        }
      }
    }
    onBackPressedDispatcher.addCallback(this, cb)
  }
}
