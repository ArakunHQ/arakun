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

    /** A pairing link opened from elsewhere, or a notification tapped. */
    private fun opened(intent: Intent?) {
        when (intent?.action) {
            Intent.ACTION_VIEW -> intent.data?.let { model.openLink(pairingText(it)) }
            // Another app could send this too: the session is only a name the
            // computer is asked for, so the worst is a session that is not there.
            Push.OPEN_SESSION -> intent.getStringExtra(Push.EXTRA_SESSION)?.takeIf { it.isNotEmpty() }?.let(model::openFromNotice)
        }
    }

    companion object {
        /**
         * `arakun://pair?link=<link>` carries a link Arakun showed. The app
         * reaches computers by account now: a link to a computer's own page
         * opens in the browser once the person says yes; any other gets a sentence.
         */
        fun pairingText(uri: Uri): String =
            // An opaque `arakun:x` has no query; asking it for one throws.
            if (uri.scheme == "arakun" && uri.isHierarchical) uri.getQueryParameter("link") ?: uri.toString() else uri.toString()
    }
}
