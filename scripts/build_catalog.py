"""Build the documented catalog from extract_typelibs.ps1's read-only snapshot.

Usage: python scripts/build_catalog.py [raw.json] [catalog.json]
No third-party Python dependencies; no COM calls or project changes.
"""
import collections
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
RAW = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ['TEMP']) / 'controlexpert_typelibs_raw.json'
OUT = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / 'control_expert_api_catalog.json'
raw = json.loads(RAW.read_text(encoding='utf-8-sig'))
INFRA = {'QueryInterface', 'AddRef', 'Release', 'GetTypeInfoCount', 'GetTypeInfo', 'GetIDsOfNames', 'Invoke'}
LIB_INFO = {
    'UnityProBroker': ('ude', 'Creation and retrieval of Control Expert sessions and authentication context.'),
    'UnityProServer': ('ude', 'Object model for applications, projects, program, variables, blocks, hardware and DTM.'),
    'PSExeLib': ('ude', 'Connectors to Control Expert runtime environment instances.'),
    'PSNOTIFYLib': ('ude', 'Server notifications and change subscriptions.'),
    'PSRDtmLib': ('ude_extension', 'Secondary IProject3 interface for accessing the DTM tree.'),
    'UnityProLibset': ('libraries', 'Catalogs of library set versions, libraries, families and DDT, DFB and EFB types.'),
    'UnitySecurityServer': ('security', 'Users, profiles, permissions, certificates and policies of the security server.'),
    'UnitySecurityServerInternal': ('internal', 'Interface identified as internal by its library name.'),
    'UnityFdtCatalogServerLib': ('fdt_support', 'Access to the FDT catalog server installed with Control Expert.'),
    'LoggerLib': ('simulator_support', 'Simulator logging and tracing interfaces.'),
    'CsConfigMgr': ('security_support', 'Configuration manager and access rights interfaces.'),
    'APPLICATIONFORUNITTESTLib': ('tests', 'Interfaces identified as unit tests by their name.'),
    'APPLICATIONSAFETYLib': ('ude_extension', 'Application extension for Safety functions and passwords.'),
    'PSRConfigurationLib': ('ude_extension', 'Secondary IConfiguration2 interface for accessing internal buses.'),
    'PRIVATEDATALib': ('private', 'Interface identified as private data by its name.'),
    'PRIVATEINTERFACElib': ('private', 'Interface identified as private by its name.'),
    'APPLICATIONPRIVATElib': ('private', 'Extension identified as private by its name.'),
}
RESTRICTED_CATEGORIES = {'private', 'internal', 'tests'}
NOUNS = {
    'Name': 'the name', 'Comment': 'the comment', 'Id': 'the identifier', 'Version': 'the version',
    'Count': 'the number of elements', 'Project': 'the active project', 'ProjectFileName': 'the project path',
    'CurrentSignature': 'the current signature', 'InfoBuildState': 'the build state',
    'InfoCreation': 'the creation information', 'InfoModification': 'the modification information',
    'Variables': 'the variables collection', 'Program': 'the program', 'Configuration': 'the configuration',
    'Documentation': 'the documentation', 'FctModules': 'the functional modules', 'FctViews': 'the functional views',
    'Networks': 'the networks', 'Ddts': 'the derived data types (DDT)', 'Dfbs': 'the derived function blocks (DFB)',
    'Efbs': 'the elementary function blocks (EFB)', 'IODdts': 'the input/output data types (IODDT)',
    'Tasks': 'the tasks', 'Sections': 'the sections', 'Srs': 'the subroutines', 'IOEvents': 'the I/O events',
    'TimerEvents': 'the timer events', 'ProgramUnits': 'the program units', 'Pous': 'the POUs',
    'AnimationTables': 'the animation tables', 'AnimTables': 'the animation tables',
    'Resources': 'the resources', 'Context': 'the object context', 'Cpu': 'the configured CPU',
    'Family': 'the family', 'PartNumber': 'the part number', 'TopoAddress': 'the topological address',
    'TopologicalAddress': 'the topological address', 'InitialValue': 'the initial value', 'TypeName': 'the type name',
    'TypeCategory': 'the type category', 'TypeCode': 'the type code', 'Language': 'the programming language',
    'Periodicity': 'the periodicity', 'WatchDog': 'the watchdog time', 'Protection': 'the protection',
    'PrivateInfo': 'the private information', 'DTMRoot': 'the root of the DTM tree', 'InternalBuses': 'the internal buses',
    'Drops': 'the remote drops', 'Racks': 'the racks', 'Modules': 'the modules', 'DtmId': 'the DTM identifier',
    'DtmType': 'the DTM type', 'Dtms': 'the DTM collection', 'ChildrenList': 'the list of children',
    'LocalChild': 'the local child', 'Library': 'the library', 'LibSetVersion': 'the library set version',
    'LibraryPath': 'the library path', 'CustomLibraryPath': 'the custom library path',
    'OutputWindow': 'the output window', 'TargetState': 'the PLC state',
    'TargetSignature': 'the target signature', 'TargetConnectionState': 'the target connection state',
    'TargetConnectionMode': 'the target connection mode', 'InterfaceVersion': 'the interface version',
    'InstallationPath': 'the installation path', 'ExecutionPath': 'the execution path',
    'SimulatorConnectionAddress': 'the simulator connection address',
    'SimulatorConnectionDriver': 'the simulator connection driver',
    'PlcConnectionAddress': 'the PLC connection address', 'PlcConnectionDriver': 'the PLC connection driver',
    'SlaveBusAddress': 'the slave bus address', 'NumberOfClients': 'the number of clients',
    'UserName': 'the user name', 'UserPassword': 'the user password', 'SafetyProjectPassword': 'the project Safety password',
    'Profile': 'the profile', 'Namespace': 'the namespace', 'Type': 'the type', 'State': 'the state',
    'Enabled': 'the enabled state', 'Value': 'the value', 'Description': 'the description', 'Thumbprint': 'the certificate thumbprint',
    'Key': 'the key', 'FileName': 'the file name', 'AliasName': 'the alias', 'FamilyName': 'the family name',
    'InfoSize': 'the size information', 'InfoSignature': 'the signature', 'InfoVersion': 'the version',
    'ValidationDate': 'the validation date', 'ModificationDate': 'the modification date',
    'IsVisible': 'the visibility', 'IsModified': 'the modification state', 'IsProjectOpen': 'the project open state',
    'IsMain': 'the main instance flag', 'SubLstDtms': 'the list of subordinate DTMs',
    'SubLstFctModules': 'the list of subordinate functional modules', 'ProgFctModule': 'the functional module program',
    'AnimationTableFctModule': 'the functional module animation tables', 'Logic': 'the logic',
    'LaunchContext': 'the launch context', 'XpdfContext': 'the XPDF context',
    'SecDBPath': 'the security database path', 'SecurityDatabasePath': 'the security database path',
    'GenLock': 'the generation lock (GenLock)', 'Printer': 'the selected printer',
    'DescriptiveForm': 'the descriptive form of the type', 'Localization': 'the object localization',
    'PathDescription': 'the path description', 'FileSignature': 'the file signature',
    'TopoNumber': 'the topological number', 'CanChangeApp': 'whether the application can be changed',
    'AppName': 'the application name', 'CurrentVersion': 'the current version',
    'MaxSecurityMode': 'the maximum security mode', 'LoginType': 'the login type',
    'Model': 'the security model', 'AuditFlag': 'the audit flag', 'ConfirmFlag': 'the confirmation flag',
    'Audit': 'the audit setting', 'Confirm': 'the confirmation setting', 'CatId': 'the category identifier',
    'DistinguishedName': 'the certificate distinguished name', 'CertType': 'the certificate type',
    'AuthentificationMode': 'the authentication mode', 'CentralizedAuthTimeout': 'the centralized authentication timeout',
    'CentralizedAuthDefaultProfileName': 'the centralized authentication default profile name',
    'CentralizedAuthProtocol': 'the centralized authentication protocol',
    'LDAPServerIP': 'the LDAP server IP address', 'LDAPServerPort': 'the LDAP server port',
    'LDAPBaseDN': 'the LDAP base DN',
}
METHODS = {
    'OpenApplication': 'Opens a Control Expert application from the given path.',
    'NewApplication': 'Creates a Control Expert application session.',
    'OpenApplicationWithProfile': 'Opens an application with the given profile and user.',
    'NewApplicationWithProfile': 'Creates an application with the given profile and user.',
    'OpenApplicationWithPServerBrokerContext': 'Opens an application using a broker context.',
    'NewApplicationWithPServerBrokerContext': 'Creates an application using a broker context.',
    'NewProject': 'Creates a project with the given family, CPU and version.',
    'OpenAPX': 'Opens a project from the given file.',
    'OpenAPXWithLibsetVersion': 'Opens a project with the given library set version.',
    'OpenArchive': 'Opens an archived project file.',
    'OpenArchiveWithLibsetVersion': 'Opens an archived project file with a library set version.',
    'ImportProject': 'Imports a project from the given file.',
    'CloseProject': 'Closes the project.', 'ProjectClose': 'Closes the project.',
    'Build': 'Builds the project changes.', 'BuildAll': 'Rebuilds and builds the whole project.',
    'ReBuildAll': 'Rebuilds and builds the whole project.', 'Analyze': 'Analyzes the project.',
    'Save': 'Saves the object or project.', 'SaveAs': 'Saves the project to another path.',
    'SaveArchive': 'Saves an archived project file.',
    'TransferAllPCtoPLC': 'Transfers the whole application from the PC to the PLC.',
    'TransferAllPLCToPC': 'Transfers the whole application from the PLC to the PC.',
    'TargetConnectionOpen': 'Opens the connection to the selected target.',
    'TargetConnectionClose': 'Closes the connection to the target.',
    'TargetSendCommand': 'Sends the PLC command given by the input enumeration.',
    'SelectTarget': 'Selects the connection target.', 'SetSelectTarget': 'Selects the connection target.',
    'ShowHMI': 'Shows the Control Expert user interface.', 'HideHMI': 'Hides the Control Expert user interface.',
    'GetDisplayPosition': 'Gets the position and size of the Control Expert window.',
    'SetDisplayPosition': 'Sets the position, size and state of the Control Expert window.',
    'DisplayStart': 'Starts displaying the application.', 'DisplayStop': 'Stops displaying the application.',
    'DisplayStartControl': 'Starts displaying through a host control.',
    'Reserve': 'Reserves access to the object.', 'Dispose': 'Releases the application resources.',
    'ExitPServer': 'Terminates the application server session.',
    'GetContentsAsString': 'Gets the output window contents as text.',
    'GetContentsAsArray': 'Gets the output window contents as an array.',
    'GetContentsAsStringWithErr': 'Gets the contents and error information as text.',
    'GetOutputSheetWithError': 'Gets the output sheet that contains errors.',
    'GetOutpuSheetContentsAsString': 'Gets the contents of an output sheet as text.',
    'Add': 'Adds an element to the collection.', 'Delete': 'Deletes the object.',
    'Item': 'Gets an element of the collection matching the given criteria.',
    '_NewEnum': 'Returns the COM enumerator of the collection.', '_Enum': 'Returns the collection enumerator.',
    'Enum': 'Enumerates elements.', 'Import': 'Imports the object or collection from the given file.',
    'ImportWithStrategy': 'Imports content using the given strategy.',
    'Export': 'Exports the object or collection to the given file.',
    'ExportXVM': 'Exports the variables in XVM format.',
    'ExportWithFilter': 'Exports the variables matching the given filter.',
    'ExportGivenVariables': 'Exports the given variables.',
    'ExportGivenVariablesIntoDOM': 'Exports the given variables into a DOM document.',
    'ExportGivenSections': 'Exports the given sections.',
    'ExportWithDifferences': 'Exports content including differences.',
    'Search': 'Searches objects by type and name and returns the matches.',
    'ListModifiedById': 'Lists the elements modified between the given signatures.',
    'ListModifiedByIdWithOMC': 'Lists the modified elements including OMC information.',
    'ListDeletedById': 'Lists the identifiers of objects deleted between two signatures.',
    'ListDeletedWithInfos': 'Lists deleted objects with additional information.',
    'ListAttributesSupported': 'Lists the supported attributes.',
    'ListAttributesValues': 'Lists attribute values of the selected elements.',
    'ListAttributesReturnValues': 'Gets the values returned by the attribute query.',
    'GetListAttributesValues': 'Gets the values of the given attributes.',
    'SetListAttributesValues': 'Sets the values of the given attributes.',
    'GetVarImplem': 'Gets the representation or implementation of the variable.',
    'DisplayEditor': 'Opens the object editor.', 'CloseEditor': 'Closes the object editor.',
    'DisplayEditorControl': 'Shows the object editor in a host control.',
    'CloseEditorControl': 'Closes the object editor hosted in a control.',
    'DisplayDefaultEditor': 'Shows the default editor of the object.',
    'DisplayDiagnosisEditor': 'Shows the diagnosis editor of the object.',
    'CloseAllEditor': 'Closes the open editors.',
    'GetVersion': 'Gets the components of the project version.',
    'PutVersion': 'Sets the components of the project version.',
    'SaveDataFromPLC': 'Saves data read from the PLC to a file.',
    'RestoreDataToPLC': 'Restores the data from the given file to the PLC.',
    'PutPrivateFile': 'Adds a private file to the project.',
    'GetPrivateFiles': 'Lists the private files of the project matching a filter.',
    'GetTmpPath': 'Gets the temporary path of the project.',
    'GetProjectSettingValue': 'Gets the value of a project setting.',
    'IsEditorValidationRequired': 'Checks whether editors are pending validation.',
    'IsEditorValidationRequiredDisableMsgBox': 'Checks whether editors require validation, without a dialog box.',
    'Move': 'Changes the position of an element within its collection.',
    'MoveToTask': 'Moves the object to the given task.',
    'GetActivationCondition': 'Gets the activation condition of the object.',
    'SetActivationCondition': 'Sets the activation condition of the object.',
    'GetControlParameters': 'Gets the control parameters of the object.',
    'SetControlParameters': 'Sets the control parameters of the object.',
    'GetControlParameter': 'Gets the given control parameter.',
    'SetControlParameter': 'Sets the given control parameter.',
    'AttachSectionToFctModule': 'Attaches the section to the given functional module.',
    'DetachSectionToFctModule': 'Detaches the section from its functional module.',
    'AttachToFctModule': 'Attaches the object to the given functional module.',
    'DetachToFctModule': 'Detaches the object from its functional module.',
    'GetListOfSectionsAttached': 'Lists the sections attached to the functional module.',
    'GetListOfObjectsAttached': 'Lists the objects attached to the functional module.',
    'DetachAllSections': 'Detaches all sections from the functional module.',
    'DetachAllObjects': 'Detaches all objects from the functional module.',
    'MoveToFctModule': 'Moves the object to another functional module.',
    'SetIdFatherFctModule': 'Sets the identifier of the parent functional module.',
    'AddVariable': 'Adds a variable to the animation table.',
    'DeleteVariable': 'Removes a variable from the animation table.',
    'GetTemporary': 'Checks whether the animation table is temporary.',
    'SetTemporary': 'Sets whether the animation table is temporary.',
    'Generate': 'Generates the project documentation.', 'StartPrint': 'Starts printing the documentation.',
    'SelectPrinter': 'Selects the printer.',
    'GetNetworkInfo': 'Gets the network configuration information.',
    'GetStationNumber': 'Gets the station number.', 'SetStationNumber': 'Sets the station number.',
    'GetGlobalData': 'Gets the global data configuration.', 'SetGlobalData': 'Sets the global data configuration.',
    'GetServiceMessaging': 'Gets the messaging service configuration.',
    'SetServiceMessaging': 'Sets the messaging service configuration.',
    'BreakNetlink': 'Breaks the network link.', 'ListNetwork': 'Lists the networks.',
    'Change': 'Requests a configuration change.', 'Request': 'Performs a query or request on the configuration.',
    'AddChild': 'Adds a child element to the configuration tree.',
    'DeleteChild': 'Deletes a child element from the configuration tree.',
    'MoveChild': 'Moves a child element of the configuration tree.',
    'ReplaceChild': 'Replaces a child element of the configuration tree.',
    'CopyChild': 'Copies a child element of the configuration tree.',
    'CopyChildWithName': 'Copies a child element using the given name.',
    'CopyChildOnChannelCommunication': 'Copies a child element onto a communication channel.',
    'ImportChild': 'Imports a child element into the configuration.',
    'ChangeProperties': 'Changes the properties of the hardware element.',
    'MultiChangeProperties': 'Changes several hardware properties.',
    'ChangeTask': 'Changes the task assigned to the hardware element.',
    'ChangeParam': 'Changes a parameter of the hardware element.',
    'AddCommunicationDtm': 'Adds a communication DTM.', 'AddSlaveDtm': 'Adds a slave DTM.',
    'GetDtmFromId': 'Gets a DTM by identifier.', 'GetDtmFromDtmId': 'Gets a DTM by its DTM identifier.',
    'GetDtmFromName': 'Gets a DTM by name.',
    'ExportConfiguration': 'Exports the DTM configuration.',
    'ImportConfiguration': 'Imports the DTM configuration.',
    'Connect': 'Connects the object to its target device.', 'Disconnect': 'Disconnects the object.',
    'UpdateDtmPlcLink': 'Updates the link between the DTM and the PLC.',
    'SetSlaveBusAddress': 'Sets the slave bus address of the DTM.',
    'GetSymbolAndLayoutInput': 'Gets symbol and layout information of the block.',
    'ListSubfields': 'Lists the fields that make up the type.', 'ListSubFields': 'Lists the fields that make up the type.',
    'ListArrayInfos': 'Gets the array dimension information.',
    'ListFamily': 'Lists the block families.', 'ListLibrary': 'Lists the libraries.',
    'SetPath': 'Sets the path of the library catalog.', 'ResetPath': 'Resets the path of the library catalog.',
    'Reload': 'Reloads the catalog.', 'GetAPX': 'Gets the APX project.',
    'GetCurrentProject': 'Gets the current project.',
    'GetMultipleInstanceInterfaces': 'Gets the interfaces of the server instances.',
    'GetMultipleInstanceCount': 'Gets the number of server instances.',
    'GetServerNames': 'Gets the server names.',
    'IsAlive': 'Checks whether the server is available.',
    'Subscribe': 'Subscribes a receiver to notifications.', 'UnSubscribe': 'Cancels a notification subscription.',
    'GetProcessID': 'Gets the process identifier.', 'GetUnityProPID': 'Gets the Control Expert process identifier.',
    'GetProfile': 'Gets the session profile.', 'PutProfile': 'Sets the session profile.',
    'SetPasswordOnEvent': 'Supplies the password requested during an application event.',
    'SetFileEncryptionPasswordOnEvent': 'Supplies the encryption password requested during an event.',
    'EnableSafetyProtection': 'Enables the Safety protection of the project.',
    'ChangeSafetyPassword': 'Changes the Safety password of the project.',
    'SwitchToSafetyMode': 'Requests the switch to Safety mode.',
    'SwitchToMaintenanceMode': 'Requests the switch to maintenance mode.',
    'SafetyTargetConnectionOpen': 'Opens a Safety connection to the target.',
    'ChangeZEFPassword': 'Changes the ZEF file password.',
    'CheckSafetyProjectPassword': 'Checks the Safety password of the project.',
    'Login': 'Authenticates the user with the given credentials.',
    'VerifyPassword': 'Verifies the given password.', 'ChangePassword': 'Changes a password.',
    'RunTest': 'Runs a unit test.', 'Validate': 'Validates the object.',
    'ChangeApplication': 'Notifies or requests an application change to the subscription.',
    'DisplayStartForMultiWriter': 'Starts displaying for multi-writer access.',
    'AllowSwitchOfflineOnModification': 'Configures whether to go offline on modifications.',
    'ExternalViewDead': 'Notifies that an external view is no longer available.',
    'ReleaseMapOfDeleteObject': 'Releases the map of deleted objects.',
    'AnalyzeSFCStepIDs': 'Analyzes SFC step identifiers.',
    'ChangeASF': 'Requests the ASF change of the module; the parameter meaning needs additional documentation.',
    'ContinueToManageIdOfMenu': 'Continues managing the connector menu identifiers.',
    'ExternalNoMoreClients': 'Notifies that no external clients remain.',
    'ExternalNewProjectService': 'Asks the external service to create a project.',
    'ExternalImportProjectService': 'Asks the external service to import a project.',
    'ExternalUploadProjectService': 'Asks the external service to upload a project from the PLC.',
    'ExternalAppliInModification': 'Notifies that an external application is being modified.',
    'KillProcess': 'Requests termination of the given process.',
    'SendJob': 'Sends a job to the notifier.', 'Initialize': 'Initializes the object.',
    'Close': 'Closes the session or object.',
}
EVENTS = {
    'BeforeBuildComplete': 'Notification before the build completes.',
    'AfterBuildComplete': 'Notification after the build completes.',
    'WriteAccessAvailable': 'Notifies that write access is available.',
    'BeforeDownload': 'Notification before downloading to the PLC.',
    'AfterDownload': 'Notification after downloading to the PLC.',
    'AfterSave': 'Notifies that saving finished.', 'AfterSaveAs': 'Notifies saving under another name.',
    'OnLine': 'Notifies the switch to online state.', 'OffLine': 'Notifies the switch to offline state.',
    'OnApplicationPasswordDetect': 'Notifies an application password request.',
    'OnFileEncryptionPasswordDetect': 'Notifies an encryption password request.',
    'AppliModified': 'Notifies application modifications.', 'GenlockBroken': 'Notifies that GenLock was broken.',
    'DisplayStart': 'Notifies that display started.', 'DisplayStop': 'Notifies that display stopped.',
}
# ID suffix and kind of each accessor; callbacks keep their suffix but use kind=event.
KINDS = {'INVOKE_FUNC': 'method', 'INVOKE_PROPERTYGET': 'get',
         'INVOKE_PROPERTYPUT': 'put', 'INVOKE_PROPERTYPUTREF': 'putref'}
VT = {'VT_BSTR':'BSTR', 'VT_I4':'LONG', 'VT_I2':'SHORT', 'VT_UI4':'ULONG', 'VT_UI2':'USHORT',
      'VT_I1':'CHAR', 'VT_UI1':'BYTE', 'VT_R4':'FLOAT', 'VT_R8':'DOUBLE', 'VT_BOOL':'VARIANT_BOOL',
      'VT_INT':'INT', 'VT_UINT':'UINT', 'VT_VARIANT':'VARIANT', 'VT_VOID':'void',
      'VT_HRESULT':'HRESULT', 'VT_UNKNOWN':'IUnknown*', 'VT_DISPATCH':'IDispatch*', 'VT_DATE':'DATE',
      'VT_CY':'CY', 'VT_DECIMAL':'DECIMAL', 'VT_LPSTR':'LPSTR', 'VT_LPWSTR':'LPWSTR'}

def type_text(t):
    if t['kind'] == 'VT_PTR': return type_text(t['element']) + '*'
    if t['kind'] == 'VT_SAFEARRAY': return 'SAFEARRAY(' + type_text(t['element']) + ')'
    return t.get('name') or VT.get(t['kind'], t['kind'])

def base_type(t):
    while 'element' in t: t = t['element']
    return t

def useful_doc(doc, name):
    return bool(doc and re.sub(r'[^a-z0-9]', '', doc.lower()) not in {
        name.lower(), 'method'+name.lower(), 'property'+name.lower(), 'put'+name.lower()})

def describe(name, kind, iface, doc):
    core = re.sub(r'^(get_|put_)', '', name)
    if kind == 'event':
        return EVENTS.get(name, f'Notification {name} of interface {iface}.'), 'name_interpretation'
    noun = NOUNS.get(core)
    if kind in {'get', 'put', 'putref'} and noun:
        return ('Gets ' if kind == 'get' else 'Sets ') + noun + '.', 'name_interpretation'
    if name in METHODS: return METHODS[name], 'name_interpretation'
    if useful_doc(doc, name): return doc, 'typelib'
    if noun:
        return 'Accesses ' + noun + '.', 'name_interpretation'
    # Older security interfaces use PBS/UBS/InfoS prefixes. Decode only
    # explicit verbs; retain the domain token when its semantics are unknown.
    security = re.match(r'^(?:PBS|UBS|InfoS)(?:CurModel|CurProfile)?(Get|Set|FindFirst|FindNext|FindClose|Add|Del|Delete|Create|Open|Close|Save|Is|Verify)(.+)$', core)
    if security:
        verb, obj = security.groups()
        verbs={'Get':'Gets','Set':'Sets','FindFirst':'Finds the first element of',
               'FindNext':'Finds the next element of','FindClose':'Ends the search of',
               'Add':'Adds','Del':'Deletes','Delete':'Deletes','Create':'Creates','Open':'Opens',
               'Close':'Closes','Save':'Saves','Is':'Checks the existence or state of','Verify':'Verifies'}
        return f'{verbs[verb]} {NOUNS.get(obj,obj)} through {iface}.', 'name_interpretation'
    for prefix, verb in [('Get','Gets'),('Set','Sets'),('Put','Sets'),('Is','Checks'),
                         ('Has','Checks'),('List','Lists'),('Add','Adds'),('Delete','Deletes'),
                         ('Remove','Removes'),('Open','Opens'),('Close','Closes'),('Check','Checks'),
                         ('Create','Creates'),('Export','Exports'),('Import','Imports'),('Update','Updates')]:
        if core.startswith(prefix) and len(core)>len(prefix):
            obj = core[len(prefix):]
            return f'{verb} {NOUNS.get(obj, obj)} in {iface}.', 'name_interpretation'
    return f'Operation {name} of {iface}; the type library does not include its detailed semantics.', 'signature_without_semantic_documentation'

def parameter(p):
    flags = p['flags_value']
    direction = 'inout' if flags & 3 == 3 else 'out' if flags & 2 else 'in' if flags & 1 else 'unspecified'
    result = {'name':p['name'], 'position':p['position'], 'type':type_text(p['type']),
              'com_type':p['type'], 'direction':direction, 'optional':bool(flags & 16),
              'is_retval':bool(flags & 8), 'is_lcid':bool(flags & 4),
              'generated_name':bool(re.fullmatch(r'arg\d+',p['name'])), 'com_flags':p['flags']}
    if 'default' in p: result['default_value'] = p['default']
    if 'default_read_error' in p: result['default_value_read_error'] = p['default_read_error']
    return result

libs = raw['libraries']
interfaces, functions, enums, aliases, classes, libraries = [], [], [], [], [], []
type_by_name = {(l['name'], t['documentation']['name']):t for l in libs for t in l['types']}
# Coclasses identify default interfaces returned by properties typed as classes.
class_defaults = {}
event_guids = set()
for lib in libs:
    for t in lib['types']:
        if t['kind'] == 'TKIND_COCLASS':
            for impl in t['implemented_types']:
                if 'FSOURCE' in impl['flags']: event_guids.add(impl['guid'])
                if 'FDEFAULT' in impl['flags'] and 'FSOURCE' not in impl['flags']:
                    class_defaults[(lib['name'],t['documentation']['name'])] = impl['name']

def target_interfaces(t):
    b = base_type(t)
    if b['kind'] != 'VT_USERDEFINED': return []
    ln, n = b.get('library'), b['name']
    target = type_by_name.get((ln, n))
    if target and target['kind'] in ('TKIND_INTERFACE','TKIND_DISPATCH'): return [ln+'.'+n]
    default = class_defaults.get((ln,n))
    return [ln+'.'+default] if default else []

edges = collections.defaultdict(list)
roots = {}
for lib in libs:
    for t in lib['types']:
        if t['kind'] != 'TKIND_COCLASS': continue
        for reg in raw['registration'].get(t['guid'],[]):
            # The broker is the useful application entry point. Registered
            # child coclasses are not substitutes for objects in a project.
            preferred_root = (lib['name'], t['documentation']['name']) in {
                ('UnityProBroker','PServerBroker'), ('UnityProBroker','PServerBrokerContext'),
                ('UnityProLibset','RootLibsetServer'), ('UnitySecurityServer','SecurityServer'),
                ('UnitySecurityServer','Security'), ('UnityFdtCatalogServerLib','UnityFdtCatalogServerObject'),
                ('LoggerLib','LogServ'), ('CsConfigMgr','CsConfigMgr')}
            if reg and reg.get('ProgID') and preferred_root:
                for impl in t['implemented_types']:
                    if 'FDEFAULT' in impl['flags'] and 'FSOURCE' not in impl['flags']:
                        roots[lib['name']+'.'+impl['name']] = f'CreateObject("{reg["ProgID"]}")'
for lib in libs:
    for t in lib['types']:
        origin = lib['name']+'.'+t['documentation']['name']
        for f in t['functions']:
            if f['documentation']['name'] in INFRA: continue
            targets = target_interfaces(f['return_type'])
            for p in f['parameters']:
                if p['flags_value'] & 2: targets += target_interfaces(p['type'])
            for target in targets:
                edges[origin].append((target, f['documentation']['name'], f['invoke_kind'], f['parameters']))
# Secondary interfaces that Control Expert clients obtain through QueryInterface.
secondary = {
    'UnityProServer.IProject': ['UnityProServer.IProject2','PSRDtmLib.IProject3'],
    'UnityProServer.IProject2': ['PSRDtmLib.IProject3'],
    'UnityProServer.IConfiguration': ['PSRConfigurationLib.IConfiguration2'],
    'UnityProServer.ISection': ['UnityProServer.ISection2','UnityProServer.ISFCSection'],
}
access = {k:{'expression':v,'source':'registry_and_type_graph','validated_at_runtime':False} for k,v in roots.items()}
queue = collections.deque(roots)
while queue:
    origin=queue.popleft()
    # Prefer existing-object lookup to collection mutation in example paths.
    ordered_edges=sorted(edges[origin], key=lambda e: (0 if e[1]=='Item' else 1 if e[2]=='INVOKE_PROPERTYGET' else 3 if e[1].startswith(('Add','New','Create')) else 2))
    for target, name, inv, ps in ordered_edges:
        if target not in access:
            ins = [p['name'] for p in ps if p['flags_value'] & 1]
            call = name + ('(' + ', '.join('<'+p+'>' for p in ins) + ')' if inv=='INVOKE_FUNC' or ins else '')
            access[target]={'expression':access[origin]['expression']+'.'+call,'source':'registry_and_type_graph','validated_at_runtime':False}
            queue.append(target)
    for target in secondary.get(origin,[]):
        if target not in access:
            interface_name=target.split('.')[-1]
            access[target]={'expression':access[origin]['expression']+f'.QueryInterface(IID_{interface_name})',
                            'source':'secondary_interface; verify_support_on_object', 'validated_at_runtime':False}
            queue.append(target)

for lib in libs:
    ln=lib['name']
    category, summary=LIB_INFO[ln]
    is_private=category in RESTRICTED_CATEGORIES
    lib_functions=[]
    real_file=Path(re.sub(r'\\\d+$','',lib['path']))
    location={'file':lib['path'],'library':ln,'library_guid':lib['guid'],'library_version':lib['version']}
    library={'id':ln,'description':summary,'category':category,'original_documentation':lib['documentation'],
             'location':lib['path'],'equivalent_locations':[p for p in lib.get('alternate_paths',[]) if p],
             'guid':lib['guid'],'version':lib['version'],'platform':lib['platform'],'lcid':lib['lcid'],
             'file_sha256':hashlib.sha256(real_file.read_bytes()).hexdigest(),
             'public_support_confirmed':None,
             'support_warning': 'The name identifies a private, internal or test interface.' if is_private else 'Detected in the installation; presence in a type library does not guarantee contractual support from Schneider.'}
    for t in lib['types']:
        tn=t['documentation']['name']; tid=ln+'.'+tn
        loc=dict(location,type_index=t['index'],type=tn,type_guid=t['guid'])
        if t['kind'] in {'TKIND_INTERFACE','TKIND_DISPATCH'}:
            # IIOEvents and ITimerEvents are project collections, not callbacks.
            event = t['guid'] in event_guids or tn in {'IPSRApplicationEvents','IPSEConnectorEvents'}
            interface={'id':tid,'name':tn,'guid':t['guid'],'category':category,
                       'original_description':t['documentation']['text'],'com_type':t['kind'],
                       'com_flags':t['flags'],'location':loc,'base_interfaces':t['implemented_types'],
                       'is_event_interface':event,
                       'access':access.get(tid,{'expression':None,'source':'no_typed_path_from_a_registered_coclass',
                                              'note':'Obtain the object through another contract or implement an event sink.'}),
                       'functions':[], 'omitted_inherited_com_members':[]}
            for f in t['functions']:
                name=f['documentation']['name']
                if name in INFRA:
                    interface['omitted_inherited_com_members'].append(name); continue
                kind='event' if event else KINDS[f['invoke_kind']]
                fid=tid+'.'+name+':'+KINDS[f['invoke_kind']]
                description, description_source=describe(name,kind,tn,f['documentation']['text'])
                ps=[parameter(p) for p in f['parameters']]
                returned={'type':type_text(f['return_type']),'com_type':f['return_type'],
                          'is_void':f['return_type']['kind']=='VT_VOID',
                          'is_hresult_status':f['return_type']['kind']=='VT_HRESULT'}
                params_text=', '.join('['+p['direction']+(', optional' if p['optional'] else '')+'] '+p['type']+' '+p['name'] for p in ps)
                fn={'id':fid,'name':name,'interface':tid,'category':category,'kind':kind,
                    'description':description,'description_source':description_source,
                    'original_documentation':f['documentation']['text'],
                    'signature':returned['type']+' '+tn+'.'+name+'('+params_text+')',
                    'io':{'inputs':[p['name'] for p in ps if p['direction'] in {'in','inout'}],
                          'outputs':[p['name'] for p in ps if p['direction'] in {'out','inout'}],
                          'unspecified_direction':[p['name'] for p in ps if p['direction']=='unspecified'],
                          'parameters':ps,'return':returned},
                    'location':dict(loc,member=name,function_index=f['index']),
                    'com':{'dispid':f['dispid'],'invoke_kind':f['invoke_kind'],'function_kind':f['function_kind'],
                           'flags':f['flags'],'optional_parameter_count':f['optional_parameter_count'],
                           'calling_convention':f['calling_convention'],
                           'restricted':bool(f['flags_value'] & 1),'hidden':bool(f['flags_value'] & 64)},
                    'validation':{'signature':'extracted_from_local_typelib','executed':False},
                    'needs_additional_documentation':description_source=='signature_without_semantic_documentation' or not useful_doc(f['documentation']['text'],name)}
                if event:
                    fn['event_note']='Server callback to the sink; inputs are data received by the sink. Not an invocable business command.'
                functions.append(fn); interface['functions'].append(fid); lib_functions.append(fid)
            interfaces.append(interface)
        elif t['kind']=='TKIND_ENUM':
            enum_aliases=[at['documentation']['name'] for at in lib['types']
                          if at['kind']=='TKIND_ALIAS' and at['alias_type'].get('name')==tn]
            enums.append({'id':tid,'name':enum_aliases[0] if enum_aliases else tn,
                          'internal_name':tn,'aliases':enum_aliases,'location':loc,
                          'values':[{'name':v['documentation']['name'],'value':v['value'],
                                     'type':type_text(v['type']),'original_description':v['documentation']['text']} for v in t['variables']]})
        elif t['kind']=='TKIND_ALIAS':
            aliases.append({'id':tid,'name':tn,'type':type_text(t['alias_type']),'com_type':t['alias_type'],'location':loc})
        elif t['kind']=='TKIND_COCLASS':
            classes.append({'id':tid,'name':tn,'category':category,'clsid':t['guid'],'location':loc,'com_flags':t['flags'],
                            'interfaces':t['implemented_types'],
                            'registration':[r for r in raw['registration'].get(t['guid'],[]) if r],
                            'note':'A declared coclass is not necessarily creatable with CoCreateInstance; check its registration and flags.'})
        else:
            raise AssertionError('Uncatalogued type: '+t['kind'])
    library['function_count']=len(lib_functions)
    libraries.append(library)

# A setter may omit the name of the value argument. Keep the generated argN
# explicit instead of guessing a parameter name from a different accessor.
counts=collections.Counter(f['id'] for f in functions)
assert all(c==1 for c in counts.values()), [k for k,v in counts.items() if v>1]
fnids=set(counts)
for iface in interfaces:
    assert all(fid in fnids for fid in iface['functions'])
    assert len(iface['functions']) + len(iface['omitted_inherited_com_members']) == type_by_name[tuple(iface['id'].split('.',1))]['declared_function_count']
assert sum(t['declared_function_count'] for l in libs for t in l['types']) == len(functions) + sum(len(i['omitted_inherited_com_members']) for i in interfaces)
assert sum(t['declared_variable_count'] for l in libs for t in l['types']) == sum(len(e['values']) for e in enums)
assert not raw['discovery_failures'], raw['discovery_failures']
catalog={
    'schema_version':'2.0.0',
    'title':'EcoStruxure Control Expert 15.1 API catalog',
    'surveyed_on':datetime.now(timezone(timedelta(hours=-3))).date().isoformat(), 'primary_language':'en',
    'product':{'name':'EcoStruxure Control Expert','installed_version':'15.1',
               'directory':'C:\\Program Files (x86)\\Schneider Electric\\Control Expert 15.1',
               'technology':'COM/UDE (Unity Developer’s Edition)',
               'main_entry_point':'PSBroker.PServerBroker.1'},
    'scope':{
        'includes':'Every specific member of the 17 surveyed type libraries: methods, property accessors, events, interfaces, coclasses, aliases and enumerations. Includes detected secondary extensions and auxiliary libraries.',
        'completeness':'Complete with respect to the libraries listed in this file. It does not guarantee every possible interface of every Control Expert version or optional component.',
        'excludes':['Internal functions without COM metadata; arbitrary native DLL exports.',
                    'Every third-party COM library installed by FDT/DTM.',
                    'IEC EF/EFB/DFB blocks as if they were automation methods; their catalogs are reached through UnityProLibset and UnityProServer.',
                    'Complete PLC protocol specifications (Modbus/UMAS) or XML exchange schemas.'],
        'intentional_omissions':{'infrastructure_com_members':sorted(INFRA),
                                 'reason':'Listed per interface but not repeated as business operations.'},
        'documentation_status':'The original text of each type library is preserved; interpreted descriptions are marked. The signature does not specify the internal schema of every VARIANT nor every precondition.',
        'support':'Private, internal and tests are categories derived from names. Technical detection does not establish an API publicly supported by Schneider.',
        'validation':'Static metadata read with LoadTypeLibEx(REGKIND_NONE). Control Expert was not instantiated and no project or PLC operation was invoked.'},
    'how_to_read':{
        'functions':'Flat list; id = library.interface.member:accessor (method, get, put or putref). Getters and setters have separate entries. Callbacks of event interfaces have kind=event.',
        'io':'inputs and outputs contain names; parameters keeps order, direction, COM type, optionality and default value when present. inout appears in both groups.',
        'return':'The return declared by ITypeInfo. On dispatch interfaces it may be the logical Automation value; on vtable interfaces it may be HRESULT with the result travelling as an [out, retval] parameter.',
        'types':'BSTR=Unicode string; LONG=32-bit COM integer; SHORT=16 bits; VARIANT_BOOL=COM boolean; VARIANT=dynamic value; SAFEARRAY=COM array; *=pointer. User-defined types include the library to avoid name collisions.',
        'location':'Type library path or numbered DLL resource DLL\\n, GUID, type index, interface name and member. Indices are zero-based.',
        'access':'Indicative path inferred from the registry and typed returns. <parameters> are placeholders; paths are not tested code. QueryInterface may be needed for secondary interfaces.',
        'events':'Callbacks the client must implement; they are not automation commands.',
        'documentation':'original_documentation is extracted from the vendor. description_source distinguishes type library text, name interpretation and absence of detailed semantics.',
        'errors':'The type library has no exhaustive error contract. Check HRESULT/EXCEPINFO when invoking; no codes or JSON output schemas are invented.'},
    'sources':[
        {'type':'local_primary','description':'Schneider type libraries installed locally; sources, versions and hashes in libraries.'},
        {'type':'local_primary','description':'COM registration in the Registry32 and Registry64 views, queried by the CLSID of the extracted coclasses.'},
        {'type':'official_reference','title':'Unity Developer’s Edition — Schneider Electric',
         'url':'https://www.se.com/sg/en/product-range/630-unity-developers-edition/'}],
    'summary':{'libraries':len(libraries),'interfaces':len(interfaces),'functions_and_accessors':len(functions),
               'by_kind':dict(collections.Counter(f['kind'] for f in functions)),
               'by_category':dict(collections.Counter(f['category'] for f in functions)),
               'enumerations':len(enums),'enumerated_values':sum(len(e['values']) for e in enums),
               'aliases':len(aliases),'coclasses':len(classes),
               'interfaces_with_access_path':sum(i['access']['expression'] is not None for i in interfaces),
               'operations_pending_semantic_documentation':sum(f['needs_additional_documentation'] for f in functions)},
    'libraries':libraries,'interfaces':interfaces,'functions':functions,'enumerations':enums,'aliases':aliases,'coclasses':classes,
    'verification':{'extraction_errors':raw['discovery_failures'], 'unique_ids':True,
                    'member_count_vs_typelibs':'matches', 'constant_count_vs_typelibs':'matches',
                    'interface_function_references':'verified', 'business_invocations_performed':0},
    'rebuild':{'step_1':'powershell -NoProfile -File scripts/extract_typelibs.ps1',
               'step_2':'python scripts/build_catalog.py',
               'requirements':'Windows, Control Expert 15.1 installed, Windows PowerShell with Add-Type and Python 3; no additional Python dependencies.'},
}
OUT.write_text(json.dumps(catalog,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'file':str(OUT),'bytes':OUT.stat().st_size,'summary':catalog['summary']},ensure_ascii=False,indent=2))
