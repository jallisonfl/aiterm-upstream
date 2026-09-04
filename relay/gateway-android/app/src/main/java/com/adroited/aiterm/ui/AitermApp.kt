package com.adroited.aiterm.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.adroited.aiterm.ui.theme.AitermTheme
import com.adroited.aiterm.AitermApplication
import com.adroited.aiterm.AppContainer
import com.adroited.aiterm.pairing.PairedDesktop
import kotlinx.serialization.Serializable
import androidx.navigation.toRoute

/** Type-safe navigation destinations. */
@Serializable
object DesktopsRoute

@Serializable
object PairingRoute

@Serializable
data class TerminalRoute(val deviceId: String)

internal fun initialDestination(desktops: List<PairedDesktop>): Any =
    desktops.singleOrNull()?.let { TerminalRoute(it.deviceId) } ?: DesktopsRoute

/** The navigation shell. Locked launches render the safe welcome surface above this graph. */
@Composable
fun AitermApp(
    navController: NavHostController = rememberNavController(),
    onRequestUnlock: () -> Unit = {},
    unlockError: String? = null,
    dependencies: AppContainer? = null,
) {
    val application = LocalContext.current.applicationContext as AitermApplication
    val container = dependencies ?: application.container
    val locked by container.appLock.isLocked.collectAsStateWithLifecycle()

    AitermTheme {
        if (locked) {
            LockedContent(onUnlock = onRequestUnlock, error = unlockError)
        } else {
            val desktops = runCatching { container.pairedDesktopStore.all() }.getOrDefault(emptyList())
            NavHost(navController = navController, startDestination = initialDestination(desktops)) {
                composable<DesktopsRoute> {
                    DesktopListScreen(
                        store = container.pairedDesktopStore,
                        onPairDesktop = { navController.navigate(PairingRoute) },
                        onOpenDesktop = { navController.navigate(TerminalRoute(it.deviceId)) },
                    )
                }
                composable<PairingRoute> {
                    PairingScreen(
                        repository = container.pairingRepository,
                        onBack = { navController.popBackStack() },
                        onPaired = { navController.popBackStack() },
                    )
                }
                composable<TerminalRoute> { entry ->
                    val route = entry.toRoute<TerminalRoute>()
                    val desktop = runCatching { container.pairedDesktopStore.all() }
                        .getOrDefault(emptyList())
                        .firstOrNull { it.deviceId == route.deviceId }
                    if (desktop == null) {
                        LaunchedEffect(route.deviceId) {
                            if (!navController.popBackStack()) navController.navigate(DesktopsRoute)
                        }
                    } else {
                        val remoteViewModel: RemoteTerminalViewModel = viewModel(
                            key = "remote-${desktop.deviceId}",
                            factory = RemoteTerminalViewModel.factory(
                                desktop,
                                container.deviceKeys,
                                container.appLock,
                                container.pairedDesktopStore,
                                application,
                            ),
                        )
                        RemoteDesktopScreen(
                            viewModel = remoteViewModel,
                            desktop = desktop,
                            pairedDesktops = runCatching { container.pairedDesktopStore.all() }
                                .getOrDefault(listOf(desktop)),
                            onBack = {
                                if (!navController.popBackStack()) navController.navigate(DesktopsRoute)
                            },
                            onOpenDesktop = { target ->
                                navController.navigate(TerminalRoute(target.deviceId)) {
                                    popUpTo(entry.destination.id) { inclusive = true }
                                }
                            },
                            onPairDesktop = { navController.navigate(PairingRoute) },
                            onForgetDesktop = {
                                runCatching {
                                    container.pairedDesktopStore.remove(desktop.deviceId)
                                    val remaining = container.pairedDesktopStore.all()
                                    val only = remaining.singleOrNull()
                                    if (only == null) {
                                        navController.navigate(DesktopsRoute) {
                                            popUpTo(entry.destination.id) { inclusive = true }
                                        }
                                    } else {
                                        navController.navigate(TerminalRoute(only.deviceId)) {
                                            popUpTo(entry.destination.id) { inclusive = true }
                                        }
                                    }
                                }.isSuccess
                            },
                            keyBarPreference = container.terminalKeyBarPreference,
                        )
                    }
                }
            }
        }
    }
}
