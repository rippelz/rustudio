package com.rustudio.app

import android.app.NativeActivity
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import android.widget.Toast
import androidx.core.content.FileProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import java.io.File

class DiagnosticLogProvider : FileProvider()

/**
 * Hosts librustudio.so (android.app.lib_name in the manifest): the Rust side
 * (src/android.rs) is started by the NativeActivity glue. This class only
 * handles what needs Java: the storage permission and immersive mode.
 */
class MainActivity : NativeActivity() {
    private var asked = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        asked = getSharedPreferences("rustudio", Context.MODE_PRIVATE).getBoolean("asked_storage", false)
        immersive()
    }

    override fun onResume() {
        super.onResume()
        immersive()
        askStorageOnce()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) immersive()
    }

    /** Called by the native Help menu. Grants access to the selected recipient only. */
    fun shareDiagnosticLogs() = runOnUiThread {
        @Suppress("DEPRECATION")
        val shared = File(Environment.getExternalStorageDirectory(), "RuStudio/Logs")
        val privateLogs = File(filesDir, "RuStudio/Logs")
        val logs = listOf(shared, privateLogs)
            .flatMap { it.listFiles()?.asList().orEmpty() }
            .filter { it.isFile && it.length() > 0 && LOG_NAME.matches(it.name) }
            .sortedByDescending { it.lastModified() }
            .take(5)
        if (logs.isEmpty()) {
            Toast.makeText(this, R.string.diagnostic_logs_empty, Toast.LENGTH_SHORT).show()
            return@runOnUiThread
        }
        try {
            val uris = ArrayList(logs.map {
                FileProvider.getUriForFile(this, "$packageName.diagnostic-logs", it)
            })
            val action = if (uris.size == 1) Intent.ACTION_SEND else Intent.ACTION_SEND_MULTIPLE
            val share = Intent(action).apply {
                type = "text/plain"
                if (uris.size == 1) {
                    putExtra(Intent.EXTRA_STREAM, uris.first())
                } else {
                    putParcelableArrayListExtra(Intent.EXTRA_STREAM, uris)
                }
                clipData = ClipData.newUri(contentResolver, getString(R.string.diagnostic_logs), uris.first()).apply {
                    uris.drop(1).forEach { addItem(ClipData.Item(it)) }
                }
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            startActivity(Intent.createChooser(share, getString(R.string.diagnostic_logs)))
        } catch (_: RuntimeException) {
            Toast.makeText(this, R.string.diagnostic_logs_error, Toast.LENGTH_LONG).show()
        }
    }

    private companion object {
        val LOG_NAME = Regex("rustudio-[0-9]{8}-[0-9]{6}-[0-9]{3}-[0-9]+\\.log")
    }

    private fun immersive() {
        try {
            val controller = WindowInsetsControllerCompat(window, window.decorView)
            controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            controller.hide(WindowInsetsCompat.Type.systemBars())
        } catch (_: Throwable) {
        }
    }

    /** "All files access" (Android 11+) / the legacy storage permission, asked once. */
    private fun askStorageOnce() {
        if (asked) return
        asked = true
        getSharedPreferences("rustudio", Context.MODE_PRIVATE).edit().putBoolean("asked_storage", true).apply()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (Environment.isExternalStorageManager()) return
            Toast.makeText(this, R.string.storage_hint, Toast.LENGTH_LONG).show()
            try {
                startActivity(
                    Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:$packageName"))
                )
            } catch (_: Throwable) {
                try {
                    startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                } catch (_: Throwable) {
                }
            }
        } else {
            requestPermissions(
                arrayOf(
                    android.Manifest.permission.READ_EXTERNAL_STORAGE,
                    android.Manifest.permission.WRITE_EXTERNAL_STORAGE
                ), 1
            )
        }
    }
}
