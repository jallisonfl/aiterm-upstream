package com.fivelime.aiterm

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import com.fivelime.aiterm.ui.AitermApp
import com.fivelime.aiterm.ui.AitermTheme

class MainActivity : ComponentActivity() {
    private val vm: AppViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Diag.attach(applicationContext)
        enableEdgeToEdge()
        handlePairIntent(intent)
        setContent { AitermTheme { AitermApp(vm) } }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handlePairIntent(intent)
    }

    /** The QR is an aiterm://pair link: the camera app can hand it straight here. */
    private fun handlePairIntent(intent: Intent?) {
        val data = intent?.dataString ?: return
        if (data.startsWith("aiterm://pair")) vm.pair(data)
    }

    override fun onStart() { super.onStart(); vm.onStart() }
    override fun onStop() { super.onStop(); vm.onStop() }
}
