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
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen

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

  // Splash stays until the page commits; without this the starting window
  // dismisses at the first (empty) WebView frame and the launch flashes dark.
  @Volatile private var pageCommitted = false
  private val splashHandler = android.os.Handler(android.os.Looper.getMainLooper())

  override fun onCreate(savedInstanceState: Bundle?) {
    val splash = installSplashScreen()
    splash.setKeepOnScreenCondition { !pageCommitted }
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
    splashHandler.removeCallbacksAndMessages(null)
    super.onDestroy()
  }

  // Releases the splash once the Solid boot screen mounts (progress 100 alone
  // can predate first paint). No WebViewClient override, so wry IPC is intact.
  // Timeout guarantees the splash never traps the launch if a load stalls.
  private fun dismissSplashWhenCommitted(webView: WebView) {
    val handler = splashHandler
    val start = android.os.SystemClock.uptimeMillis()
    val jsPoll = object : Runnable {
      override fun run() {
        if (pageCommitted) return
        if (android.os.SystemClock.uptimeMillis() - start > 6000) {
          pageCommitted = true
          return
        }
        try {
          webView.evaluateJavascript("!!document.querySelector('.boot-screen')") { result ->
            if (pageCommitted) return@evaluateJavascript
            if (result == "true") {
              try {
                webView.postVisualStateCallback(1, object : WebView.VisualStateCallback() {
                  override fun onComplete(requestId: Long) { pageCommitted = true }
                })
              } catch (t: Throwable) {
                pageCommitted = true
              }
            } else {
              handler.postDelayed(this, 100)
            }
          }
        } catch (t: Throwable) {
          handler.postDelayed(this, 100)
        }
      }
    }
    val poll = object : Runnable {
      override fun run() {
        if (pageCommitted) return
        if (webView.progress >= 100 ||
            android.os.SystemClock.uptimeMillis() - start > 6000) {
          handler.post(jsPoll)
          return
        }
        handler.postDelayed(this, 50)
      }
    }
    handler.post(poll)
  }

  // Forward the Android back button / back gesture (gesture nav + 3-button)
  // into the web layer. window.__androidBack() returns "true" when the app
  // consumed the press (closed an overlay or stepped up a level); otherwise
  // fall through to the OS so the app backgrounds/exits from the top level.
  override fun onWebViewCreate(webView: WebView) {
    // Dark ground from the first frame: the starting window is already gone by
    // the time the page paints, so an empty WebView would flash black instead.
    webView.setBackgroundColor(0xFF0C0D12.toInt())
    dismissSplashWhenCommitted(webView)
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
