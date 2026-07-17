package com.sonus.cosmog

import android.content.Context

object NativeBridge {
    init {
        System.loadLibrary("cosmog_lib")
    }

    @JvmStatic
    external fun initNdkContext(context: Context)
}
