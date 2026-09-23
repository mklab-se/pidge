// pidge remote MCP server — Azure infrastructure.
//
// One resource group, one Container App, and the least infrastructure that
// still meets the bar: RBAC-only Key Vault for per-user refresh tokens and
// the token-signing key, a user-assigned managed identity as the only
// principal that can read them, ACR pulls via that same identity (no admin
// user), and logs in Log Analytics.
//
// Deployed in two phases by deploy.sh: first without `image` (so the registry
// exists to build into), then with it.

targetScope = 'resourceGroup'

@description('Azure region. Defaults to the resource group location.')
param location string = resourceGroup().location

@description('Short project name used as the naming stem.')
@minLength(3)
@maxLength(10)
param baseName string = 'pidge'

@description('Deployment environment label, e.g. spike, prod.')
param environment string = 'spike'

@description('Fully qualified container image to run. Empty skips the Container App (phase 1).')
param image string = ''

@description('Comma-separated list of Microsoft account e-mails allowed to sign in.')
param allowedEmails string

@description('Object id of a person who should be able to manage secrets in the vault (optional).')
param vaultAdminObjectId string = ''

@description('Log verbosity for the server (RUST_LOG syntax).')
param logLevel string = 'info,pidge_mcp=debug'

var suffix = uniqueString(resourceGroup().id)
var tags = {
  project: baseName
  environment: environment
  'managed-by': 'bicep'
  repository: 'github.com/mklab-se/pidge'
}

var appName = 'ca-${baseName}-mcp'
var appPort = 8080

// Built-in role definition ids.
var roleKeyVaultSecretsOfficer = 'b86a8fe4-44ce-4948-aee5-eccb2c155cd7'
var roleAcrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'

// ---------------------------------------------------------------------------
// Observability
// ---------------------------------------------------------------------------

resource logs 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: 'log-${baseName}'
  location: location
  tags: tags
  properties: {
    sku: {
      name: 'PerGB2018'
    }
    retentionInDays: 30
    features: {
      enableLogAccessUsingOnlyResourcePermissions: true
    }
  }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: 'id-${baseName}-mcp'
  location: location
  tags: tags
}

// ---------------------------------------------------------------------------
// Container registry (pulls via managed identity, admin user disabled)
// ---------------------------------------------------------------------------

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: 'cr${baseName}${suffix}'
  location: location
  tags: tags
  sku: {
    name: 'Basic'
  }
  properties: {
    adminUserEnabled: false
    publicNetworkAccess: 'Enabled'
  }
}

resource registryPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(registry.id, identity.id, roleAcrPull)
  scope: registry
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleAcrPull)
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
  }
}

// ---------------------------------------------------------------------------
// Key Vault (RBAC authorization, soft delete on)
// ---------------------------------------------------------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: 'kv-${baseName}-${suffix}'
  location: location
  tags: tags
  properties: {
    tenantId: subscription().tenantId
    sku: {
      family: 'A'
      name: 'standard'
    }
    enableRbacAuthorization: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 30
    // Purge protection is deliberately off while this is a spike so the
    // resource group can be torn down cleanly. Turn it on before real use.
    enablePurgeProtection: null
    publicNetworkAccess: 'Enabled'
    networkAcls: {
      bypass: 'AzureServices'
      defaultAction: 'Allow'
    }
  }
}

resource vaultSecretsOfficerApp 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(vault.id, identity.id, roleKeyVaultSecretsOfficer)
  scope: vault
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleKeyVaultSecretsOfficer)
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
  }
}

resource vaultSecretsOfficerHuman 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(vaultAdminObjectId)) {
  name: guid(vault.id, vaultAdminObjectId, roleKeyVaultSecretsOfficer)
  scope: vault
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roleKeyVaultSecretsOfficer)
    principalId: vaultAdminObjectId
    principalType: 'User'
  }
}

// ---------------------------------------------------------------------------
// Container Apps environment (consumption)
// ---------------------------------------------------------------------------

resource containerEnv 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: 'cae-${baseName}'
  location: location
  tags: tags
  properties: {
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: logs.properties.customerId
        sharedKey: logs.listKeys().primarySharedKey
      }
    }
    workloadProfiles: [
      {
        name: 'Consumption'
        workloadProfileType: 'Consumption'
      }
    ]
  }
}

// ---------------------------------------------------------------------------
// The MCP server
// ---------------------------------------------------------------------------

var publicUrl = 'https://${appName}.${containerEnv.properties.defaultDomain}'

resource app 'Microsoft.App/containerApps@2024-03-01' = if (!empty(image)) {
  name: appName
  location: location
  tags: tags
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${identity.id}': {}
    }
  }
  properties: {
    managedEnvironmentId: containerEnv.id
    workloadProfileName: 'Consumption'
    configuration: {
      activeRevisionsMode: 'Single'
      ingress: {
        external: true
        targetPort: appPort
        transport: 'auto'
        allowInsecure: false
      }
      registries: [
        {
          server: registry.properties.loginServer
          identity: identity.id
        }
      ]
    }
    template: {
      containers: [
        {
          name: 'pidge-mcp'
          image: image
          resources: {
            cpu: json('0.25')
            memory: '0.5Gi'
          }
          env: [
            { name: 'PORT', value: string(appPort) }
            { name: 'RUST_LOG', value: logLevel }
            { name: 'PIDGE_MCP_PUBLIC_URL', value: publicUrl }
            { name: 'PIDGE_MCP_ALLOWED_EMAILS', value: allowedEmails }
            { name: 'PIDGE_MCP_KEYVAULT_URL', value: vault.properties.vaultUri }
            { name: 'AZURE_CLIENT_ID', value: identity.properties.clientId }
          ]
          probes: [
            {
              type: 'Liveness'
              httpGet: {
                path: '/healthz'
                port: appPort
              }
              initialDelaySeconds: 3
              periodSeconds: 30
            }
            {
              type: 'Startup'
              httpGet: {
                path: '/healthz'
                port: appPort
              }
              initialDelaySeconds: 1
              periodSeconds: 2
              failureThreshold: 15
            }
          ]
        }
      ]
      scale: {
        // A single replica keeps the short-lived in-memory OAuth state
        // (pending authorizations, used codes) coherent. Scale-to-zero is
        // fine: a Rust binary cold-starts in well under a second.
        minReplicas: 0
        maxReplicas: 1
      }
    }
  }
  dependsOn: [
    registryPull
    vaultSecretsOfficerApp
  ]
}

output registryName string = registry.name
output registryLoginServer string = registry.properties.loginServer
output keyVaultName string = vault.name
output keyVaultUri string = vault.properties.vaultUri
output identityClientId string = identity.properties.clientId
output publicUrl string = publicUrl
output appFqdn string = empty(image) ? '' : app!.properties.configuration.ingress.fqdn
