// Detour.c: Windows HID detour facilities

#include <Driver.h>
#include "Detour.tmh"

#ifdef ALLOC_PRAGMA
#pragma alloc_text (PAGE, PtpFilterDetourWindowsHIDStack)
#endif

NTSTATUS
PtpFilterDetourWindowsHIDStack(
    _In_ WDFDEVICE Device
)
{
    NTSTATUS status = STATUS_SUCCESS;
    PDEVICE_CONTEXT deviceContext;
    PDEVICE_OBJECT  hidTransportWdmDeviceObject = NULL;
    PDRIVER_OBJECT  hidTransportWdmDriverObject = NULL;
    PIO_CLIENT_EXTENSION hidTransportIoClientExtension = NULL;
    PHIDCLASS_DRIVER_EXTENSION hidTransportClassExtension = NULL;

    PAGED_CODE();
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "%!FUNC! Entry");
    deviceContext = PtpFilterGetContext(Device);

    if (deviceContext->WdmDeviceObject == NULL || deviceContext->WdmDeviceObject->DriverObject == NULL) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "%!FUNC! WDM Device Object or Driver Object is null, can't continue");
        status = STATUS_UNSUCCESSFUL;
        goto exit;
    }

    // Access the driver object to find next low-level device (in our case, we expect it to be HID transport driver)
    hidTransportWdmDeviceObject = IoGetLowerDeviceObject(deviceContext->WdmDeviceObject);
    if (hidTransportWdmDeviceObject == NULL) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "%!FUNC! IoGetLowerDeviceObject returns null, can't continue");
        status = STATUS_UNSUCCESSFUL;
        goto exit;
    }

    hidTransportWdmDriverObject = hidTransportWdmDeviceObject->DriverObject;
    if (hidTransportWdmDriverObject == NULL) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "%!FUNC! DriverObject of HID transport Device Object is null, can't continue");
        status = STATUS_UNSUCCESSFUL;
        goto cleanup;
    }

    // Verify if the driver extension is what we expected.
    // C28175: reading _DRIVER_OBJECT::DriverExtension is intentional here -- this routine
    // deliberately reaches into the HID transport driver object to install the detour.
#pragma warning(suppress: 28175)
    if (hidTransportWdmDriverObject->DriverExtension == NULL) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "%!FUNC! DriverExtension of HID transport Driver Object is null, can't continue");
        status = STATUS_UNSUCCESSFUL;
        goto cleanup;
    }

    // Just two more check...
    // C28175: deliberate access to _DRIVER_OBJECT::DriverExtension (see note above).
#pragma warning(suppress: 28175)
    hidTransportIoClientExtension = ((PDRIVER_EXTENSION_EXT)hidTransportWdmDriverObject->DriverExtension)->IoClientExtension;
    if (hidTransportIoClientExtension == NULL) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "%!FUNC! IO Extension is NULL, can't continue");
        status = STATUS_UNSUCCESSFUL;
        goto cleanup;
    }

    if (strncmp(HID_CLASS_EXTENSION_LITERAL_ID, hidTransportIoClientExtension->ClientIdentificationAddress, sizeof(HID_CLASS_EXTENSION_LITERAL_ID)) != 0) {
        TraceEvents(TRACE_LEVEL_ERROR, TRACE_DEVICE, "%!FUNC! IO Extension mismatch, can't continue");
        status = STATUS_UNSUCCESSFUL;
        goto cleanup;
    }

    hidTransportClassExtension = (PHIDCLASS_DRIVER_EXTENSION)(hidTransportIoClientExtension + 1);
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "%!FUNC! Sanity check seems okay, safe to patch IO handler routines");

    // HIDClass overrides:
    // IRP_MJ_SYSTEM_CONTROL, IRP_MJ_WRITE, IRP_MJ_READ, IRP_MJ_POWER, IRP_MJ_PNP, IRP_MJ_INTERNAL_DEVICE_CONTROL, IRP_MJ_DEVICE_CONTROL
    // IRP_MJ_CREATE, IRP_MJ_CLOSE
    // For us, overriding IRP_MJ_DEVICE_CONTROL and IRP_MJ_INTERNAL_DEVICE_CONTROL might be sufficient.
    // Details: https://ligstd.visualstudio.com/Apple%20PTP%20Trackpad/_wiki/wikis/Apple-PTP-Trackpad.wiki/47/Hijack-HIDCLASS
    // C28175: writing _DRIVER_OBJECT::MajorFunction is the whole point of the detour --
    // we redirect the HID transport's internal IOCTL dispatch to HIDCLASS's handler.
#pragma warning(suppress: 28175)
    hidTransportWdmDriverObject->MajorFunction[IRP_MJ_INTERNAL_DEVICE_CONTROL] = hidTransportClassExtension->MajorFunction[IRP_MJ_INTERNAL_DEVICE_CONTROL];
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "%!FUNC! IRP_MJ_INTERNAL_DEVICE_CONTROL patched");

    // Mark detour as completed.
    deviceContext->IsHidIoDetourCompleted = TRUE;
    deviceContext->HidIoTarget = WdfDeviceGetIoTarget(Device);

cleanup:
    if (hidTransportWdmDeviceObject != NULL) {
        ObDereferenceObject(hidTransportWdmDeviceObject);
    }
exit:
    TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_DEVICE, "%!FUNC! Exit, Status = %!STATUS!", status);
    return status;
}
