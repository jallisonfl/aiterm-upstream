package com.adroited.aiterm

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class LocalNetworkPermissionTest {

    @Test
    fun api37PackageDeclaresLocalNetworkAccessForDirectDesktopConnections() {
        assumeTrue(Build.VERSION.SDK_INT >= 37)
        val context = ApplicationProvider.getApplicationContext<Context>()
        val packageInfo = context.packageManager.getPackageInfo(
            context.packageName,
            PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS.toLong()),
        )

        assertTrue(
            "API 37 blocks direct LAN sockets until ACCESS_LOCAL_NETWORK is declared",
            Manifest.permission.ACCESS_LOCAL_NETWORK in
                packageInfo.requestedPermissions.orEmpty(),
        )
    }
}
