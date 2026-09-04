package com.adroited.aiterm.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.clickable
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.viewmodel.compose.viewModel
import com.adroited.aiterm.R
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairedDesktopStore

/**
 * Start destination: the desktops this phone trusts. Empty until Task 8 stores
 * a pairing, so the empty state carries the whole first-run instruction.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DesktopListScreen(
    store: PairedDesktopStore,
    onPairDesktop: () -> Unit,
    onOpenDesktop: (PairedDesktop) -> Unit = {},
    viewModel: DesktopListViewModel = viewModel(factory = DesktopListViewModel.factory(store)),
) {
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    var forgetTarget by remember { mutableStateOf<PairedDesktop?>(null) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { viewModel.refresh() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.desktops_title)) },
                actions = {
                    if (!uiState.storageFailure && uiState.desktops.isNotEmpty()) {
                        TextButton(onClick = onPairDesktop) {
                            Text(stringResource(R.string.action_pair_desktop))
                        }
                    }
                },
            )
        },
    ) { innerPadding ->
        if (uiState.storageFailure) {
            Column(
                modifier = Modifier.fillMaxSize().padding(innerPadding).padding(24.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text("Paired desktop storage could not be read.")
                Text(
                    "AITerm left the stored data unchanged. Pairing and reconnecting are disabled.",
                    textAlign = TextAlign.Center,
                )
            }
        } else if (uiState.desktops.isEmpty()) {
            EmptyDesktopList(
                onPairDesktop = onPairDesktop,
                modifier = Modifier.fillMaxSize().padding(innerPadding).padding(24.dp),
            )
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize().padding(innerPadding)) {
                items(uiState.desktops, key = PairedDesktop::deviceId) { desktop ->
                    ListItem(
                        modifier = Modifier.clickable { onOpenDesktop(desktop) },
                        headlineContent = { Text(desktop.displayName) },
                        supportingContent = {
                            Text(
                                desktop.serverSpkiFingerprint.chunked(4).joinToString("-"),
                            )
                        },
                        trailingContent = {
                            TextButton(onClick = { forgetTarget = desktop }) {
                                Text(stringResource(R.string.action_forget_desktop))
                            }
                        },
                    )
                }
            }
        }
    }

    forgetTarget?.let { desktop ->
        AlertDialog(
            onDismissRequest = { forgetTarget = null },
            title = {
                Text(stringResource(R.string.forget_desktop_title, desktop.displayName))
            },
            text = {
                Text(stringResource(R.string.forget_desktop_body))
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        viewModel.forget(desktop.deviceId)
                        forgetTarget = null
                    },
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) { Text(stringResource(R.string.action_confirm_forget_desktop)) }
            },
            dismissButton = {
                TextButton(onClick = { forgetTarget = null }) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

@Composable
private fun EmptyDesktopList(onPairDesktop: () -> Unit, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(text = stringResource(R.string.desktops_empty_title))
        Text(
            text = stringResource(R.string.desktops_empty_body),
            textAlign = TextAlign.Center,
        )
        Button(onClick = onPairDesktop) {
            Text(stringResource(R.string.action_pair_desktop))
        }
    }
}
