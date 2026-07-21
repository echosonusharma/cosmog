package com.sonus.cosmog

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  // Disable WryActivity's default back handling (WebView.canGoBack()/exit).
  // This SPA drives navigation from JS signal state, not WebView history, so
  // canGoBack() is always false and the OS back action would just exit.
  override val handleBackNavigation = false

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    NativeBridge.initNdkContext(applicationContext)
    super.onCreate(savedInstanceState)
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
