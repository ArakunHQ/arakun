package com.arakun.android

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import com.arakun.android.ui.ArakunTheme
import com.arakun.android.ui.Root

class MainActivity : ComponentActivity() {
    private val model: AppModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.auto(android.graphics.Color.TRANSPARENT, android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.auto(android.graphics.Color.TRANSPARENT, android.graphics.Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        model.begin(Demo.from(intent))
        if (savedInstanceState == null) opened(intent)
        lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) = model.visible(true)
            override fun onStop(owner: LifecycleOwner) = model.visible(false)
        })
        setContent {
            ArakunTheme { Root(model) }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        opened(intent)
    }

    /** A pairing link opened from elsewhere. */
    private fun opened(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        intent.data?.let { model.pair(pairingText(it)) }
    }

    companion object {
        /**
         * The QR code holds an https link to the relay; `arakun://pair?link=<it>`
         * carries the same, for a link opened from elsewhere.
         */
        fun pairingText(uri: Uri): String =
            // An opaque `arakun:x` has no query; asking it for one throws.
            if (uri.scheme == "arakun" && uri.isHierarchical) uri.getQueryParameter("link") ?: uri.toString() else uri.toString()
    }
}
